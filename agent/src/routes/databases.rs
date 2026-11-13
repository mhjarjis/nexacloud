//! MySQL / PostgreSQL listing and backup dumps.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::Json;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::util::cmd::Cmd;
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
}

impl EngineInfo {
    fn unavailable() -> Self {
        EngineInfo {
            available: false,
            version: None,
            databases: Vec::new(),
        }
    }
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
        return EngineInfo::unavailable();
    }
    let list = mysql(MYSQL_LIST_QUERY).run().await;
    EngineInfo {
        available: true,
        version: Some(clean_mysql_version(&ver.stdout)).filter(|s| !s.is_empty()),
        databases: if list.ok { parse_mysql_listing(&list.stdout) } else { Vec::new() },
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
        return EngineInfo::unavailable();
    }
    let version = ver.stdout.split_whitespace().next().map(str::to_string);
    let list = psql(&[], PG_LIST_QUERY).run().await;
    let rows = if list.ok { parse_pg_listing(&list.stdout) } else { Vec::new() };
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

pub async fn backup(
    State(state): State<AppState>,
    UrlPath((engine, name)): UrlPath<(String, String)>,
) -> ApiResult<Json<Value>> {
    let engine = validate_backup_params(&engine, &name)?;
    let listing = match engine {
        Engine::Mysql => mysql_info().await,
        Engine::Postgres => postgres_info().await,
    };
    if !listing.available {
        return Err(ApiError::bad_request(format!("{} is not available", engine.as_str())));
    }
    if !listing.databases.iter().any(|d| d.name == name) {
        return Err(ApiError::not_found(format!("database not found: {name}")));
    }
    let title = format!("Back up {}/{}", engine.as_str(), name);
    let job_id = state.jobs.create("db_backup", &title);
    let st = state.clone();
    let id = job_id.clone();
    tokio::spawn(async move {
        st.jobs.set_running(&id);
        let ok = match run_backup(&st, &id, engine, &name).await {
            Ok(()) => true,
            Err(e) => {
                st.jobs.log(&id, &format!("FAILED: {e:#}"));
                false
            }
        };
        st.jobs.finish(&id, ok);
    });
    Ok(Json(json!({ "job_id": job_id })))
}

async fn run_backup(state: &AppState, job: &str, engine: Engine, name: &str) -> anyhow::Result<()> {
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

    state.jobs.log(job, &format!("Dumping {} database {name}", engine.as_str()));
    let result = dump_pipeline(state, job, engine, name, file).await;
    match result {
        Ok(()) => {
            std::fs::rename(&tmp_path, &final_path)
                .with_context(|| format!("renaming to {}", final_path.display()))?;
            let size = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
            let rel = relative(&backup_dir, &final_path);
            state.jobs.log(job, &format!("Wrote {rel} ({size} bytes)"));
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

fn relative(base: &Path, p: &Path) -> String {
    p.strip_prefix(base)
        .map(PathBuf::from)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// `<dump> | gzip -c > file`, wired process-to-process (no shell).
async fn dump_pipeline(
    state: &AppState,
    job: &str,
    engine: Engine,
    name: &str,
    file: std::fs::File,
) -> anyhow::Result<()> {
    use anyhow::{anyhow, bail};

    let mut dump_cmd = engine.dump_command(name).command();
    dump_cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut dump = dump_cmd
        .spawn()
        .map_err(|e| anyhow!("cannot start {} dump: {e}", engine.as_str()))?;
    let dump_out: Stdio = dump
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no dump stdout"))?
        .try_into()
        .map_err(|e| anyhow!("pipe: {e}"))?;
    let mut gz_cmd = Cmd::new("gzip").arg("-c").command();
    gz_cmd
        .stdin(dump_out)
        .stdout(Stdio::from(file))
        .stderr(Stdio::null());
    let mut gzip = match gz_cmd.spawn() {
        Ok(g) => g,
        Err(e) => {
            let _ = dump.kill().await;
            bail!("cannot start gzip: {e}");
        }
    };

    // Stream dump stderr into the job log.
    let stderr_task = dump.stderr.take().map(|err| {
        let st = state.clone();
        let job = job.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            let mut n = 0;
            while let Ok(Some(line)) = lines.next_line().await {
                if n < 200 {
                    st.jobs.log(&job, &line);
                }
                n += 1;
            }
        })
    });

    let waited = tokio::time::timeout(BACKUP_TIMEOUT, async {
        let d = dump.wait().await;
        let g = gzip.wait().await;
        (d, g)
    })
    .await;
    let (d, g) = match waited {
        Ok(x) => x,
        Err(_) => {
            let _ = dump.kill().await;
            let _ = gzip.kill().await;
            bail!("timed out after {} minutes", BACKUP_TIMEOUT.as_secs() / 60);
        }
    };
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
                DbInfo { name: "shop_prod".into(), size_bytes: 1288490188, tables: 84 },
                DbInfo { name: "blog".into(), size_bytes: 0, tables: 0 },
            ]
        );
        assert_eq!(clean_mysql_version("8.0.40-0ubuntu0.24.04.1\n"), "8.0.40");
        assert_eq!(clean_mysql_version("8.4.3"), "8.4.3");
        assert_eq!(clean_mysql_version("10.11.8-MariaDB-0ubuntu0.24.04.1"), "10.11.8-MariaDB");
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
        assert_eq!(validate_backup_params("mysql", "shop_prod").unwrap(), Engine::Mysql);
        assert_eq!(validate_backup_params("postgres", "app-1").unwrap(), Engine::Postgres);
        assert!(validate_backup_params("sqlite", "x").is_err());
        assert!(validate_backup_params("mysql", "../etc").is_err());
        assert!(validate_backup_params("mysql", "a b").is_err());
        assert!(validate_backup_params("mysql", "").is_err());
        assert!(validate_backup_params("MYSQL", "x").is_err());
    }
}
