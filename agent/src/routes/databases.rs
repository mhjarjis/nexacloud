//! MySQL / PostgreSQL listing and backup dumps.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path as UrlPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::audit::{audited, AuditInfo};
use crate::error::{ApiError, ApiResult};
use crate::jobs::{JobCtx, JobSpec};
use crate::state::AppState;
use crate::util::cmd::{Cmd, CmdOutput, ProcessGroupGuard};
use crate::util::{create_private_dir, is_valid_db_name};

pub const BACKUP_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const MYSQL_SYSTEM: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];
const PG_SYSTEM: [&str; 3] = ["template0", "template1", "postgres"];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DbInfo {
    pub name: String,
    pub size_bytes: u64,
    pub tables: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineInfo {
    pub available: bool,
    pub version: Option<String>,
    pub databases: Vec<DbInfo>,
    /// Why the engine is unavailable when it is installed but unreachable
    /// (null when it simply isn't installed).
    pub error: Option<String>,
}

impl EngineInfo {
    fn unavailable(error: Option<String>) -> Self {
        EngineInfo {
            available: false,
            version: None,
            databases: Vec::new(),
            error,
        }
    }
}

/// Turn a failed client call into a message a person can act on.
pub fn explain_failure(engine: &str, out: &CmdOutput) -> Option<String> {
    if out.missing {
        return None;
    }
    if out.timed_out {
        return Some(format!("{engine} did not answer in time."));
    }
    let first = out
        .stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let lower = first.to_ascii_lowercase();
    let hint = if engine == "MySQL" && lower.contains("access denied") {
        "MySQL refused the root login. NexaCloud connects as root over the unix socket; if your root user has a password, put it in /root/.my.cnf ([client] user=root password=…, mode 600) and reload the page."
            .to_string()
    } else if lower.contains("can't connect")
        || lower.contains("could not connect")
        || lower.contains("no such file")
    {
        format!("{engine} is installed but not running.")
    } else if first.is_empty() {
        format!("{engine} client failed without an error message.")
    } else {
        format!("{engine} client error: {first}")
    };
    Some(hint)
}

#[derive(Debug, Clone, Serialize)]
pub struct DatabasesInfo {
    pub mysql: EngineInfo,
    pub postgres: EngineInfo,
}

// ------------------------------------------------------------------ mysql

fn mysql(query: &str) -> Cmd {
    Cmd::new("mysql").args(["--batch", "--skip-column-names", "-e", query])
}

const MYSQL_LIST_QUERY: &str = "SELECT s.SCHEMA_NAME, COALESCE(SUM(t.DATA_LENGTH + t.INDEX_LENGTH), 0), COUNT(t.TABLE_NAME) \
FROM information_schema.SCHEMATA s LEFT JOIN information_schema.TABLES t ON t.TABLE_SCHEMA = s.SCHEMA_NAME \
WHERE s.SCHEMA_NAME NOT IN ('information_schema','mysql','performance_schema','sys') \
GROUP BY s.SCHEMA_NAME ORDER BY s.SCHEMA_NAME";

/// `8.0.40-0ubuntu0.24.04.1` → `8.0.40`; `10.11.8-MariaDB-0ubuntu…` → `10.11.8-MariaDB`.
pub fn clean_mysql_version(v: &str) -> String {
    let v = v.trim();
    let mut parts = v.split('-');
    let base = parts.next().unwrap_or("").to_string();
    if v.contains("MariaDB") {
        format!("{base}-MariaDB")
    } else {
        base
    }
}

/// Tab-separated `name  size  tables` rows.
pub fn parse_mysql_listing(out: &str) -> Vec<DbInfo> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let name = it.next()?.trim().to_string();
            let size = it.next()?.trim().parse::<f64>().ok()? as u64;
            let tables = it.next()?.trim().parse().ok()?;
            (!name.is_empty() && !MYSQL_SYSTEM.contains(&name.as_str())).then_some(DbInfo {
                name,
                size_bytes: size,
                tables,
            })
        })
        .collect()
}

pub async fn mysql_info() -> EngineInfo {
    let ver = mysql("SELECT VERSION()").run().await;
    if !ver.ok {
        return EngineInfo::unavailable(explain_failure("MySQL", &ver));
    }
    let list = mysql(MYSQL_LIST_QUERY).run().await;
    EngineInfo {
        available: true,
        version: Some(clean_mysql_version(&ver.stdout)).filter(|s| !s.is_empty()),
        databases: if list.ok {
            parse_mysql_listing(&list.stdout)
        } else {
            Vec::new()
        },
        error: None,
    }
}

// --------------------------------------------------------------- postgres

fn psql(extra: &[&str], query: &str) -> Cmd {
    Cmd::new("runuser")
        .args(["-u", "postgres", "--", "psql", "-X", "-At"])
        .args(extra)
        .args(["-c", query])
}

const PG_LIST_QUERY: &str = "SELECT datname, pg_database_size(datname) FROM pg_database \
WHERE datallowconn AND datname NOT IN ('template0','template1','postgres') ORDER BY datname";
const PG_TABLES_QUERY: &str = "SELECT count(*) FROM information_schema.tables \
WHERE table_schema NOT IN ('pg_catalog','information_schema')";

/// `|`-separated `name|size` rows.
pub fn parse_pg_listing(out: &str) -> Vec<(String, u64)> {
    out.lines()
        .filter_map(|l| {
            let (name, size) = l.rsplit_once('|')?;
            let name = name.trim().to_string();
            let size = size.trim().parse().ok()?;
            (!name.is_empty() && !PG_SYSTEM.contains(&name.as_str())).then_some((name, size))
        })
        .collect()
}

pub async fn postgres_info() -> EngineInfo {
    let ver = psql(&[], "SHOW server_version").run().await;
    if !ver.ok {
        return EngineInfo::unavailable(explain_failure("PostgreSQL", &ver));
    }
    let version = ver.stdout.split_whitespace().next().map(str::to_string);
    let list = psql(&[], PG_LIST_QUERY).run().await;
    let rows = if list.ok {
        parse_pg_listing(&list.stdout)
    } else {
        Vec::new()
    };
    let futs = rows
        .iter()
        .map(|(name, _)| {
            let name = name.clone();
            async move {
                // Only connect to names we consider safe to pass to psql.
                if !is_valid_db_name(&name) {
                    return 0;
                }
                let dbarg = format!("--dbname={name}");
                let out = psql(&[dbarg.as_str()], PG_TABLES_QUERY).run().await;
                if out.ok {
                    out.stdout.trim().parse().unwrap_or(0)
                } else {
                    0
                }
            }
        })
        .collect();
    let tables = crate::util::join_all(futs).await;
    EngineInfo {
        available: true,
        version,
        databases: rows
            .into_iter()
            .zip(tables)
            .map(|((name, size_bytes), t)| DbInfo {
                name,
                size_bytes,
                tables: t.unwrap_or(0),
            })
            .collect(),
        error: None,
    }
}

pub async fn info() -> DatabasesInfo {
    let (mysql, postgres) = tokio::join!(mysql_info(), postgres_info());
    DatabasesInfo { mysql, postgres }
}

pub async fn list() -> Json<DatabasesInfo> {
    Json(info().await)
}

// ----------------------------------------------------------------- backup

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Engine {
    Mysql,
    Postgres,
}

impl Engine {
    pub fn parse(s: &str) -> Option<Engine> {
        match s {
            "mysql" => Some(Engine::Mysql),
            "postgres" => Some(Engine::Postgres),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::Mysql => "mysql",
            Engine::Postgres => "postgres",
        }
    }
    fn dump_command(self, name: &str) -> Cmd {
        match self {
            // `--` ends option parsing so a name can never be read as a flag.
            Engine::Mysql => Cmd::new("mysqldump")
                .args(["--single-transaction", "--quick", "--routines", "--"])
                .arg(name),
            Engine::Postgres => Cmd::new("runuser")
                .args(["-u", "postgres", "--", "pg_dump", "--no-password"])
                .arg(format!("--dbname={name}")),
        }
    }
}

/// Validate the backup path params. Returns the engine on success.
pub fn validate_backup_params(engine: &str, name: &str) -> Result<Engine, ApiError> {
    let engine = Engine::parse(engine)
        .ok_or_else(|| ApiError::bad_request("engine must be mysql or postgres"))?;
    if !is_valid_db_name(name) {
        return Err(ApiError::bad_request("invalid database name"));
    }
    Ok(engine)
}

/// `POST /api/databases/{engine}/{name}/backup` → `{"job_id"}`. Runs as a
/// heavy job (one dump at a time) holding the `db:<engine>/<name>` lock.
pub async fn backup(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    UrlPath((engine, name)): UrlPath<(String, String)>,
) -> Response {
    let mut info = AuditInfo::action("db.backup", format!("{engine}/{name}"));
    let res = backup_inner(&state, addr, &engine, &name).await;
    if let Ok(id) = &res {
        info.job_id = Some(id.clone());
    }
    audited(
        res.map(|id| Json(json!({ "job_id": id })).into_response())
            .unwrap_or_else(IntoResponse::into_response),
        info,
    )
}

async fn backup_inner(
    state: &AppState,
    addr: SocketAddr,
    engine: &str,
    name: &str,
) -> ApiResult<String> {
    let engine = validate_backup_params(engine, name)?;
    let listing = match engine {
        Engine::Mysql => mysql_info().await,
        Engine::Postgres => postgres_info().await,
    };
    if !listing.available {
        return Err(ApiError::bad_request(format!(
            "{} is not available",
            engine.as_str()
        )));
    }
    if !listing.databases.iter().any(|d| d.name == name) {
        return Err(ApiError::not_found(format!("database not found: {name}")));
    }
    let title = format!("Back up {}/{}", engine.as_str(), name);
    let spec = JobSpec::new("db_backup", &title)
        .heavy()
        .lock(format!("db:{}/{name}", engine.as_str()))
        .timeout(BACKUP_TIMEOUT)
        .actor(Some(addr.ip().to_canonical().to_string()));
    let st = state.clone();
    let name = name.to_string();
    let id = state.jobs.submit(spec, move |ctx| async move {
        run_backup(&st, &ctx, engine, &name).await
    })?;
    Ok(id)
}

/// Removes the partial dump file unless the backup completed.
struct PartialFile(Option<PathBuf>);

impl Drop for PartialFile {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

async fn run_backup(
    state: &AppState,
    job: &JobCtx,
    engine: Engine,
    name: &str,
) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::os::unix::fs::OpenOptionsExt;

    let backup_dir = state.config.backup_dir.clone();
    let dir = backup_dir.join(engine.as_str()).join(name);
    for d in [&backup_dir, &backup_dir.join(engine.as_str()), &dir] {
        create_private_dir(d).with_context(|| format!("creating {}", d.display()))?;
    }
    let stamp = Utc::now().format("%Y%m%d-%H%M%S");
    let final_path = dir.join(format!("{name}-{stamp}.sql.gz"));
    let tmp_path = dir.join(format!(".{name}-{stamp}.sql.gz.partial"));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp_path)
        .with_context(|| format!("creating {}", tmp_path.display()))?;
    // Cleans up on failure, timeout or cancel (the future is dropped).
    let mut partial = PartialFile(Some(tmp_path.clone()));

    job.log(&format!("Dumping {} database {name}", engine.as_str()));
    dump_pipeline(job, engine, name, file).await?;
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("renaming to {}", final_path.display()))?;
    partial.0 = None;
    let size = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
    let rel = relative(&backup_dir, &final_path);
    job.log(&format!("Wrote {rel} ({size} bytes)"));
    Ok(())
}

fn relative(base: &Path, p: &Path) -> String {
    p.strip_prefix(base)
        .map(PathBuf::from)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// `<dump> | gzip -c > file`, wired process-to-process (no shell). Both
/// processes share one process group, killed if this future is dropped
/// (job cancelled or timed out).
async fn dump_pipeline(
    job: &JobCtx,
    engine: Engine,
    name: &str,
    file: std::fs::File,
) -> anyhow::Result<()> {
    use anyhow::{anyhow, bail};

    let mut dump_cmd = engine.dump_command(name).command();
    dump_cmd
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut dump = dump_cmd
        .spawn()
        .map_err(|e| anyhow!("cannot start {} dump: {e}", engine.as_str()))?;
    let mut group = ProcessGroupGuard::new(dump.id());
    let pgid = dump.id().and_then(|p| i32::try_from(p).ok()).unwrap_or(0);
    let dump_out: Stdio = dump
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no dump stdout"))?
        .try_into()
        .map_err(|e| anyhow!("pipe: {e}"))?;
    let mut gz_cmd = Cmd::new("gzip").arg("-c").command();
    gz_cmd
        .process_group(pgid)
        .stdin(dump_out)
        .stdout(Stdio::from(file))
        .stderr(Stdio::null());
    let mut gzip = gz_cmd
        .spawn()
        .map_err(|e| anyhow!("cannot start gzip: {e}"))?;

    // Stream dump stderr into the job log.
    let stderr_task = dump.stderr.take().map(|err| {
        let log = job.log.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            let mut n = 0;
            while let Ok(Some(line)) = lines.next_line().await {
                if n < 200 {
                    log.line(&line);
                }
                n += 1;
            }
        })
    });

    let d = dump.wait().await;
    let g = gzip.wait().await;
    group.disarm();
    if let Some(t) = stderr_task {
        let _ = tokio::time::timeout(Duration::from_secs(5), t).await;
    }
    let d = d.map_err(|e| anyhow!("waiting for dump: {e}"))?;
    let g = g.map_err(|e| anyhow!("waiting for gzip: {e}"))?;
    if !d.success() {
        bail!("{} dump exited with {d}", engine.as_str());
    }
    if !g.success() {
        bail!("gzip exited with {g}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_parsing() {
        let out = "shop_prod\t1288490188\t84\nblog\t0\t0\nmysql\t100\t30\nbad line\n";
        let v = parse_mysql_listing(out);
        assert_eq!(
            v,
            vec![
                DbInfo {
                    name: "shop_prod".into(),
                    size_bytes: 1288490188,
                    tables: 84
                },
                DbInfo {
                    name: "blog".into(),
                    size_bytes: 0,
                    tables: 0
                },
            ]
        );
        assert_eq!(clean_mysql_version("8.0.40-0ubuntu0.24.04.1\n"), "8.0.40");
        assert_eq!(clean_mysql_version("8.4.3"), "8.4.3");
        assert_eq!(
            clean_mysql_version("10.11.8-MariaDB-0ubuntu0.24.04.1"),
            "10.11.8-MariaDB"
        );
    }

    #[test]
    fn pg_parsing() {
        let out = "app|8123456\npostgres|7000000\ntemplate1|1\nweird|name|42\n";
        assert_eq!(
            parse_pg_listing(out),
            vec![("app".to_string(), 8123456), ("weird|name".to_string(), 42)]
        );
    }

    #[test]
    fn backup_param_validation() {
        assert_eq!(
            validate_backup_params("mysql", "shop_prod").unwrap(),
            Engine::Mysql
        );
        assert_eq!(
            validate_backup_params("postgres", "app-1").unwrap(),
            Engine::Postgres
        );
        assert!(validate_backup_params("sqlite", "x").is_err());
        assert!(validate_backup_params("mysql", "../etc").is_err());
        assert!(validate_backup_params("mysql", "a b").is_err());
        assert!(validate_backup_params("mysql", "").is_err());
        assert!(validate_backup_params("MYSQL", "x").is_err());
    }
}

#[cfg(test)]
mod explain_tests {
    use super::explain_failure;
    use crate::util::cmd::CmdOutput;

    fn failed(stderr: &str) -> CmdOutput {
        CmdOutput {
            ok: false,
            missing: false,
            timed_out: false,
            stdout: String::new(),
            stderr: stderr.into(),
            ..Default::default()
        }
    }

    #[test]
    fn missing_client_is_not_an_error() {
        let out = CmdOutput {
            missing: true,
            ..Default::default()
        };
        assert_eq!(explain_failure("MySQL", &out), None);
    }

    #[test]
    fn access_denied_points_to_my_cnf() {
        let msg = explain_failure("MySQL", &failed("ERROR 1045 (28000): Access denied for user 'root'@'localhost' (using password: NO)")).unwrap();
        assert!(msg.contains("/root/.my.cnf"), "{msg}");
    }

    #[test]
    fn socket_missing_means_not_running() {
        let msg = explain_failure("MySQL", &failed("ERROR 2002 (HY000): Can't connect to local MySQL server through socket '/var/run/mysqld/mysqld.sock' (2)")).unwrap();
        assert!(msg.contains("not running"), "{msg}");
    }
}
