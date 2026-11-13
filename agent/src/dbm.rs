//! Database manager (phpMyAdmin-style) over the local MySQL/MariaDB and
//! PostgreSQL servers, using their own CLI clients:
//!
//! - MySQL: `mysql --batch` as root over the unix socket (reads /root/.my.cnf
//!   when root has a password). Batch mode escapes `\t \n \\ \0` and prints NULL.
//! - PostgreSQL: `psql --csv` as the `postgres` OS user.
//!
//! SQL always goes through **stdin**, never argv. The console is read-only by
//! default (READ ONLY transaction + 30 s statement limit); write mode is a sudo
//! endpoint and destructive statements need the database name typed back.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Path as UrlPath, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit::{audited, AuditInfo};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::util::cmd::Cmd;
use crate::util::is_valid_db_name;

const ROW_CAP: usize = 1000;
const QUERY_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Debug, Clone, Serialize, Default)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
}

// ------------------------------------------------------------------ quoting

pub fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b == b'-')
}

fn qi_my(s: &str) -> String {
    format!("`{}`", s.replace('`', "``"))
}
fn qi_pg(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// MySQL string literal.
pub fn qv_my(v: &str) -> String {
    let mut o = String::with_capacity(v.len() + 2);
    o.push('\'');
    for c in v.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\'' => o.push_str("\\'"),
            '\0' => o.push_str("\\0"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\u{1a}' => o.push_str("\\Z"),
            c => o.push(c),
        }
    }
    o.push('\'');
    o
}

fn qv_pg(v: &str) -> String {
    format!("'{}'", v.replace('\'', "''"))
}

fn lit_my(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => {
            if *b {
                "1".into()
            } else {
                "0".into()
            }
        }
        Value::Number(n) => n.to_string(),
        Value::String(s) => qv_my(s),
        other => qv_my(&other.to_string()),
    }
}

// ------------------------------------------------------------------ runners

fn unescape_batch(field: &str) -> Option<String> {
    if field == "NULL" {
        return None;
    }
    let mut o = String::with_capacity(field.len());
    let mut it = field.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => o.push('\n'),
                Some('t') => o.push('\t'),
                Some('0') => o.push('\0'),
                Some('\\') => o.push('\\'),
                Some(x) => {
                    o.push('\\');
                    o.push(x)
                }
                None => o.push('\\'),
            }
        } else {
            o.push(c);
        }
    }
    Some(o)
}

pub fn parse_batch(out: &str) -> Table {
    let mut lines = out.lines();
    let Some(head) = lines.next() else {
        return Table::default();
    };
    let columns: Vec<String> = head
        .split('\t')
        .map(|c| unescape_batch(c).unwrap_or_default())
        .collect();
    let mut rows = Vec::new();
    let mut truncated = false;
    for l in lines {
        if rows.len() >= ROW_CAP {
            truncated = true;
            break;
        }
        rows.push(l.split('\t').map(unescape_batch).collect());
    }
    Table {
        columns,
        rows,
        truncated,
    }
}

async fn mysql(db: Option<&str>, sql: &str, timeout: Duration) -> Result<Table, String> {
    let mut c = Cmd::new("mysql").args([
        "--batch",
        "--column-names",
        "--default-character-set=utf8mb4",
        // Disable client-side commands (\\!, system, source, DELIMITER, \\g ...).
        "--skip-commands",
    ]);
    if let Some(d) = db {
        c = c.arg(format!("--database={d}"));
    }
    let o = c
        .stdin(sql.as_bytes().to_vec())
        .timeout(timeout)
        .env("HOME", "/root")
        .run()
        .await;
    if o.missing {
        return Err("MySQL client is not installed".into());
    }
    if !o.ok {
        return Err(o
            .stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("MySQL error")
            .trim()
            .to_string());
    }
    Ok(parse_batch(&o.stdout))
}

/// Minimal RFC 4180 CSV parser (psql --csv). Empty unquoted field = NULL.
pub fn parse_csv(out: &str) -> Table {
    let mut records: Vec<Vec<Option<String>>> = Vec::new();
    let mut rec: Vec<Option<String>> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    let mut chars = out.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                was_quoted = true;
            }
            ',' => {
                rec.push(if field.is_empty() && !was_quoted {
                    None
                } else {
                    Some(std::mem::take(&mut field))
                });
                was_quoted = false;
            }
            '\n' => {
                rec.push(if field.is_empty() && !was_quoted {
                    None
                } else {
                    Some(std::mem::take(&mut field))
                });
                was_quoted = false;
                records.push(std::mem::take(&mut rec));
            }
            '\r' => {}
            c => field.push(c),
        }
    }
    if !field.is_empty() || !rec.is_empty() || was_quoted {
        rec.push(if field.is_empty() && !was_quoted {
            None
        } else {
            Some(field)
        });
        records.push(rec);
    }
    let mut it = records.into_iter();
    let columns = it
        .next()
        .map(|h| h.into_iter().map(Option::unwrap_or_default).collect())
        .unwrap_or_default();
    let mut rows = Vec::new();
    let mut truncated = false;
    for r in it {
        if rows.len() >= ROW_CAP {
            truncated = true;
            break;
        }
        rows.push(r);
    }
    Table {
        columns,
        rows,
        truncated,
    }
}

async fn psql(db: &str, sql: &str, timeout: Duration) -> Result<Table, String> {
    let o = Cmd::new("runuser")
        .args([
            "-u",
            "postgres",
            "--",
            "psql",
            "-X",
            "-q",
            "--csv",
            "-v",
            "ON_ERROR_STOP=1",
            &format!("--dbname={db}"),
        ])
        .stdin(sql.as_bytes().to_vec())
        .timeout(timeout)
        .current_dir("/tmp")
        .run()
        .await;
    if o.missing {
        return Err("PostgreSQL client is not installed".into());
    }
    if !o.ok {
        return Err(o
            .stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("PostgreSQL error")
            .trim()
            .to_string());
    }
    Ok(parse_csv(&o.stdout))
}

/// Read-only PostgreSQL console: psql runs as `nobody` and connects over TCP
/// as `nexacloud_ro` (member of pg_read_all_data), so neither psql
/// meta-commands nor server-side tricks can write or run programs.
const PG_RO_PW: &str = "/etc/nexacloud/pg_ro.pw";

async fn ensure_pg_ro() -> Result<String, String> {
    if let Ok(p) = std::fs::read_to_string(PG_RO_PW) {
        if !p.trim().is_empty() {
            return Ok(p.trim().to_string());
        }
    }
    let pw = gen_password();
    psql(
        "postgres",
        &format!(
            "DO $nc$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'nexacloud_ro') THEN CREATE ROLE nexacloud_ro LOGIN; END IF; END $nc$;\nALTER ROLE nexacloud_ro LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD {};\nGRANT pg_read_all_data TO nexacloud_ro;",
            qv_pg(&pw)
        ),
        QUERY_TIMEOUT,
    )
    .await?;
    crate::util::write_private_file(std::path::Path::new(PG_RO_PW), pw.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(pw)
}

async fn psql_ro(db: &str, sql: &str, timeout: Duration) -> Result<Table, String> {
    let pw = ensure_pg_ro().await?;
    let o = Cmd::new("runuser")
        .args([
            "-u",
            "nobody",
            "--",
            "psql",
            "-X",
            "-q",
            "--csv",
            "-v",
            "ON_ERROR_STOP=1",
            &format!("host=127.0.0.1 user=nexacloud_ro dbname={db} sslmode=prefer"),
        ])
        .env("PGPASSWORD", &pw)
        .env(
            "PGOPTIONS",
            "-c default_transaction_read_only=on -c statement_timeout=30000",
        )
        .stdin(sql.as_bytes().to_vec())
        .timeout(timeout)
        .current_dir("/tmp")
        .run()
        .await;
    if !o.ok {
        return Err(o
            .stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("PostgreSQL error")
            .trim()
            .to_string());
    }
    Ok(parse_csv(&o.stdout))
}

/// PostgreSQL-aware scan: strings have no backslash escapes, `$tag$` dollar
/// quoting, `--` and `/* */` comments. Returns (normalized upper-case text
/// outside quotes, found an unquoted backslash).
pub fn normalize_pg(sql: &str) -> (String, bool) {
    let b: Vec<char> = sql.chars().collect();
    let mut out = String::new();
    let mut backslash = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            '\'' | '"' => {
                i += 1;
                while i < b.len() {
                    if b[i] == c {
                        if i + 1 < b.len() && b[i + 1] == c {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                out.push(' ');
            }
            '$' => {
                let mut j = i + 1;
                while j < b.len() && (b[j].is_alphanumeric() || b[j] == '_') {
                    j += 1;
                }
                if j < b.len() && b[j] == '$' {
                    let tag: Vec<char> = b[i..=j].to_vec();
                    let mut k = j + 1;
                    let mut found = None;
                    while k + tag.len() <= b.len() {
                        if b[k..k + tag.len()] == tag[..] {
                            found = Some(k);
                            break;
                        }
                        k += 1;
                    }
                    i = match found {
                        Some(e) => e + tag.len() - 1,
                        None => b.len(),
                    };
                    out.push(' ');
                } else {
                    out.push('$');
                }
            }
            '-' if b.get(i + 1) == Some(&'-') => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
                out.push(' ');
            }
            '/' if b.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i += 1;
                out.push(' ');
            }
            '\\' => {
                backslash = true;
                out.push(' ');
            }
            c => out.push(c.to_ascii_uppercase()),
        }
        i += 1;
    }
    (out, backslash)
}

/// Extra checks for console input. Err = refuse.
pub fn console_guard(engine: &str, sql: &str) -> Result<(), String> {
    if engine == "postgres" {
        let (n, backslash) = normalize_pg(sql);
        if backslash {
            return Err(
                "psql meta-commands (backslash commands) are not allowed in the console".into(),
            );
        }
        let body = n.trim().trim_end_matches(';').trim();
        if body.contains(';') {
            return Err("run one statement at a time".into());
        }
        let first = body.split_whitespace().next().unwrap_or("");
        if first == "COPY" || body.split_whitespace().any(|w| w == "PROGRAM") {
            return Err("COPY is not allowed in the console (use Backups to export)".into());
        }
    }
    Ok(())
}

/// The local server is MariaDB (different syntax for statement time limits).
pub fn is_mariadb() -> bool {
    static M: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *M.get_or_init(|| {
        std::process::Command::new("mysql")
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("MariaDB"))
            .unwrap_or(false)
    })
}

fn time_limit_sql() -> &'static str {
    if is_mariadb() {
        "SET SESSION max_statement_time=30;"
    } else {
        "SET SESSION MAX_EXECUTION_TIME=30000;"
    }
}

// ------------------------------------------------------------------ validation

fn engine_db(engine: &str, db: &str) -> Result<(), ApiError> {
    if !matches!(engine, "mysql" | "postgres") {
        return Err(ApiError::bad_request("engine must be mysql or postgres"));
    }
    if !is_valid_db_name(db) {
        return Err(ApiError::bad_request("invalid database name"));
    }
    Ok(())
}

fn bad(e: String) -> ApiError {
    ApiError::bad_request(e)
}

// ------------------------------------------------------------------ tables

pub async fn tables(UrlPath((engine, db)): UrlPath<(String, String)>) -> ApiResult<Json<Value>> {
    engine_db(&engine, &db)?;
    let t = if engine == "mysql" {
        mysql(None, &format!(
            "SELECT TABLE_NAME, TABLE_ROWS, DATA_LENGTH, INDEX_LENGTH, ENGINE, TABLE_TYPE FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME",
            qv_my(&db)
        ), QUERY_TIMEOUT).await.map_err(bad)?
    } else {
        psql(&db, "SELECT c.relname, c.reltuples::bigint, pg_relation_size(c.oid), pg_indexes_size(c.oid), 'postgres', CASE c.relkind WHEN 'v' THEN 'VIEW' ELSE 'BASE TABLE' END FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public' AND c.relkind IN ('r','v','p') ORDER BY 1", QUERY_TIMEOUT).await.map_err(bad)?
    };
    let list: Vec<Value> = t
        .rows
        .iter()
        .map(|r| {
            let n = |i: usize| {
                r.get(i)
                    .cloned()
                    .flatten()
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0)
                    .max(0)
            };
            json!({
                "name": r.first().cloned().flatten().unwrap_or_default(),
                "rows": n(1), "data_bytes": n(2), "index_bytes": n(3),
                "engine": r.get(4).cloned().flatten(), "type": r.get(5).cloned().flatten(),
            })
        })
        .collect();
    Ok(Json(json!(list)))
}

async fn structure_of(
    engine: &str,
    db: &str,
    table: &str,
) -> Result<(Vec<Value>, Vec<String>), String> {
    if engine == "mysql" {
        let t = mysql(None, &format!(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, COLUMN_DEFAULT, EXTRA FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} ORDER BY ORDINAL_POSITION",
            qv_my(db), qv_my(table)
        ), QUERY_TIMEOUT).await?;
        let cols: Vec<Value> = t
            .rows
            .iter()
            .map(|r| {
                json!({
                    "name": r[0], "type": r[1], "nullable": r[2].as_deref() == Some("YES"),
                    "key": r[3], "default": r[4], "extra": r[5],
                })
            })
            .collect();
        let pk = t
            .rows
            .iter()
            .filter(|r| r[3].as_deref() == Some("PRI"))
            .filter_map(|r| r[0].clone())
            .collect();
        Ok((cols, pk))
    } else {
        let t = psql(db, &format!(
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), CASE WHEN a.attnotnull THEN 'NO' ELSE 'YES' END, CASE WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY(i.indkey)) THEN 'PRI' ELSE '' END, pg_get_expr(d.adbin, d.adrelid), '' FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum WHERE a.attrelid = {}::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            qv_pg(&qi_pg(table))
        ), QUERY_TIMEOUT).await?;
        let cols: Vec<Value> = t
            .rows
            .iter()
            .map(|r| {
                json!({
                    "name": r[0], "type": r[1], "nullable": r[2].as_deref() == Some("YES"),
                    "key": r[3], "default": r[4], "extra": r[5],
                })
            })
            .collect();
        let pk = t
            .rows
            .iter()
            .filter(|r| r[3].as_deref() == Some("PRI"))
            .filter_map(|r| r[0].clone())
            .collect();
        Ok((cols, pk))
    }
}

pub async fn structure(
    UrlPath((engine, db, table)): UrlPath<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    engine_db(&engine, &db)?;
    if !valid_ident(&table) {
        return Err(ApiError::bad_request("invalid table name"));
    }
    let (cols, pk) = structure_of(&engine, &db, &table).await.map_err(bad)?;
    if cols.is_empty() {
        return Err(ApiError::not_found("table not found"));
    }
    let indexes = if engine == "mysql" {
        mysql(Some(&db), &format!("SHOW INDEX FROM {}", qi_my(&table)), QUERY_TIMEOUT).await.map(|t| {
            t.rows.iter().map(|r| json!({"name": r.get(2), "column": r.get(4), "unique": r.get(1).cloned().flatten().as_deref() == Some("0")})).collect::<Vec<_>>()
        }).unwrap_or_default()
    } else {
        psql(&db, &format!("SELECT indexname, indexdef FROM pg_indexes WHERE schemaname='public' AND tablename = {}", qv_pg(&table)), QUERY_TIMEOUT).await
            .map(|t| t.rows.iter().map(|r| json!({"name": r[0], "definition": r.get(1)})).collect()).unwrap_or_default()
    };
    Ok(Json(
        json!({"columns": cols, "primary_key": pk, "indexes": indexes}),
    ))
}

#[derive(Deserialize)]
pub struct RowsQuery {
    page: Option<usize>,
    per: Option<usize>,
    sort: Option<String>,
    dir: Option<String>,
    q: Option<String>,
}

pub async fn rows(
    UrlPath((engine, db, table)): UrlPath<(String, String, String)>,
    Query(q): Query<RowsQuery>,
) -> ApiResult<Json<Value>> {
    engine_db(&engine, &db)?;
    if !valid_ident(&table) {
        return Err(ApiError::bad_request("invalid table name"));
    }
    let (cols, pk) = structure_of(&engine, &db, &table).await.map_err(bad)?;
    let names: Vec<String> = cols
        .iter()
        .filter_map(|c| c["name"].as_str().map(String::from))
        .collect();
    if names.is_empty() {
        return Err(ApiError::not_found("table not found"));
    }
    let per = q.per.unwrap_or(50).clamp(1, 500);
    let page = q.page.unwrap_or(1).max(1);
    let my = engine == "mysql";
    let qi = |s: &str| if my { qi_my(s) } else { qi_pg(s) };
    let mut where_sql = String::new();
    if let Some(term) = q.q.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let like = format!(
            "%{}%",
            term.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let lit = if my { qv_my(&like) } else { qv_pg(&like) };
        let parts: Vec<String> = names
            .iter()
            .map(|n| {
                if my {
                    format!("CAST({} AS CHAR) LIKE {lit}", qi(n))
                } else {
                    format!("CAST({} AS TEXT) ILIKE {lit}", qi(n))
                }
            })
            .collect();
        where_sql = format!(" WHERE {}", parts.join(" OR "));
    }
    let order = match q.sort.as_deref() {
        Some(s) if names.iter().any(|n| n == s) => {
            format!(
                " ORDER BY {} {}",
                qi(s),
                if q.dir.as_deref() == Some("desc") {
                    "DESC"
                } else {
                    "ASC"
                }
            )
        }
        _ => pk
            .first()
            .map(|p| format!(" ORDER BY {}", qi(p)))
            .unwrap_or_default(),
    };
    let base = format!("FROM {}{where_sql}", qi(&table));
    let sql = format!(
        "SELECT * {base}{order} LIMIT {per} OFFSET {}",
        (page - 1) * per
    );
    let count_sql = format!("SELECT COUNT(*) {base}");
    let (data, count) = if my {
        let pre = format!("SET SESSION TRANSACTION READ ONLY; {}\n", time_limit_sql());
        (
            mysql(Some(&db), &format!("{pre}{sql}"), QUERY_TIMEOUT).await,
            mysql(Some(&db), &format!("{pre}{count_sql}"), QUERY_TIMEOUT).await,
        )
    } else {
        let pre = "SET default_transaction_read_only = on; SET statement_timeout = 30000;\n";
        (
            psql(&db, &format!("{pre}{sql}"), QUERY_TIMEOUT).await,
            psql(&db, &format!("{pre}{count_sql}"), QUERY_TIMEOUT).await,
        )
    };
    let data = data.map_err(bad)?;
    let total = count
        .ok()
        .and_then(|t| t.rows.first().cloned())
        .and_then(|r| r.first().cloned().flatten())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    Ok(Json(json!({
        "columns": data.columns, "rows": data.rows, "total": total, "page": page, "per": per,
        "primary_key": pk, "column_types": cols,
    })))
}

// ------------------------------------------------------------------ console

#[derive(Deserialize)]
pub struct QueryBody {
    sql: String,
    /// Typed database name, required for destructive statements.
    confirm: Option<String>,
}

/// Strip comments and string contents to classify a statement.
fn normalize(sql: &str) -> String {
    let mut out = String::new();
    let mut chars = sql.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '-' if chars.peek() == Some(&'-') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
                out.push(' ');
            }
            '#' => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
                out.push(' ');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                out.push(' ');
            }
            c => out.push(c.to_ascii_uppercase()),
        }
    }
    out
}

/// (single statement?, destructive?)
pub fn classify(sql: &str) -> (bool, bool) {
    classify_with(normalize(sql))
}

pub fn classify_engine(engine: &str, sql: &str) -> (bool, bool) {
    if engine == "postgres" {
        classify_with(normalize_pg(sql).0)
    } else {
        classify(sql)
    }
}

fn classify_with(n: String) -> (bool, bool) {
    let body = n.trim().trim_end_matches(';').trim();
    let single = !body.contains(';');
    let destructive = body.split(';').any(|stmt| {
        let first = stmt.split_whitespace().next().unwrap_or("");
        let has_where = stmt.split_whitespace().any(|w| w == "WHERE");
        matches!(
            first,
            "DROP" | "TRUNCATE" | "ALTER" | "RENAME" | "GRANT" | "REVOKE"
        ) || (matches!(first, "DELETE" | "UPDATE") && !has_where)
    });
    (single, destructive)
}

async fn run_console(engine: &str, db: &str, sql: &str, write: bool) -> Result<Value, String> {
    let (single, _) = classify_engine(engine, sql);
    if !single {
        return Err("run one statement at a time".into());
    }
    console_guard(engine, sql)?;
    let started = Instant::now();
    let stmt = sql.trim().trim_end_matches(';');
    let t = if engine == "mysql" {
        let pre = if write {
            format!("{}\n", time_limit_sql())
        } else {
            format!("SET SESSION TRANSACTION READ ONLY; {}\n", time_limit_sql())
        };
        mysql(
            Some(db),
            &format!("{pre}{stmt};\nSELECT ROW_COUNT() AS `__affected`;"),
            QUERY_TIMEOUT,
        )
        .await?
    } else {
        if write {
            psql(
                db,
                &format!("SET statement_timeout = 30000;\n{stmt};"),
                QUERY_TIMEOUT,
            )
            .await?
        } else {
            psql_ro(db, &format!("{stmt};"), QUERY_TIMEOUT).await?
        }
    };
    // MySQL prints the result set then the ROW_COUNT set; split them.
    let (mut table, affected) = if engine == "mysql" {
        split_affected(t)
    } else {
        (t, None)
    };
    if table.rows.len() > ROW_CAP {
        table.rows.truncate(ROW_CAP);
        table.truncated = true;
    }
    Ok(json!({
        "columns": table.columns, "rows": table.rows, "truncated": table.truncated,
        "affected": affected, "elapsed_ms": started.elapsed().as_millis() as u64, "read_only": !write,
    }))
}

fn split_affected(t: Table) -> (Table, Option<i64>) {
    // Output is header+rows for the statement (if it returns rows), then a
    // header line "__affected" and one value. In batch mode the second header
    // appears as a row in our parse.
    if t.columns == ["__affected"] {
        let a = t
            .rows
            .first()
            .and_then(|r| r.first().cloned().flatten())
            .and_then(|s| s.parse().ok());
        return (Table::default(), a);
    }
    if let Some(pos) = t
        .rows
        .iter()
        .position(|r| r.len() == 1 && r[0].as_deref() == Some("__affected"))
    {
        let a = t
            .rows
            .get(pos + 1)
            .and_then(|r| r.first().cloned().flatten())
            .and_then(|s| s.parse().ok());
        let mut rows = t.rows;
        rows.truncate(pos);
        return (
            Table {
                columns: t.columns,
                rows,
                truncated: t.truncated,
            },
            a,
        );
    }
    (t, None)
}

pub async fn query_read(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    UrlPath((engine, db)): UrlPath<(String, String)>,
    Json(b): Json<QueryBody>,
) -> Response {
    let _ = (&state, addr);
    let info = AuditInfo::action(
        "dbm.query.read",
        format!("{engine}/{db}: {}", crate::util::cmd::truncate(&b.sql, 300)),
    );
    let res = async {
        engine_db(&engine, &db)?;
        if b.sql.len() > 100_000 {
            return Err(ApiError::bad_request("query too long"));
        }
        run_console(&engine, &db, &b.sql, false)
            .await
            .map(|v| Json(v).into_response())
            .map_err(bad)
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

/// Sudo route: write mode.
pub async fn query_write(
    UrlPath((engine, db)): UrlPath<(String, String)>,
    Json(b): Json<QueryBody>,
) -> Response {
    let info = AuditInfo::action(
        "dbm.query.write",
        format!("{engine}/{db}: {}", crate::util::cmd::truncate(&b.sql, 300)),
    );
    let res = async {
        engine_db(&engine, &db)?;
        let (_, destructive) = classify_engine(&engine, &b.sql);
        if destructive && b.confirm.as_deref() != Some(db.as_str()) {
            return Err(ApiError::new(axum::http::StatusCode::PRECONDITION_REQUIRED, format!("confirm_required: this statement can destroy data. Type the database name ({db}) to run it.")));
        }
        run_console(&engine, &db, &b.sql, true).await.map(|v| Json(v).into_response()).map_err(bad)
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

// ------------------------------------------------------------------ row edits (mysql)

#[derive(Deserialize)]
pub struct RowEdit {
    #[serde(default)]
    pk: serde_json::Map<String, Value>,
    #[serde(default)]
    set: serde_json::Map<String, Value>,
}

fn where_pk(pk_cols: &[String], pk: &serde_json::Map<String, Value>) -> Result<String, ApiError> {
    if pk_cols.is_empty() {
        return Err(ApiError::bad_request(
            "this table has no primary key; use the SQL console",
        ));
    }
    let mut parts = Vec::new();
    for c in pk_cols {
        let v = pk
            .get(c)
            .ok_or_else(|| ApiError::bad_request(format!("missing primary key value for {c}")))?;
        parts.push(format!("{} = {}", qi_my(c), lit_my(v)));
    }
    Ok(parts.join(" AND "))
}

pub async fn row_action(
    UrlPath((engine, db, table, action)): UrlPath<(String, String, String, String)>,
    Json(b): Json<RowEdit>,
) -> Response {
    let info = AuditInfo::action(
        &format!("dbm.row.{action}"),
        format!("{engine}/{db}.{table}"),
    );
    let res = async {
        if engine != "mysql" {
            return Err(ApiError::bad_request(
                "row editing is available for MySQL; use the SQL console for PostgreSQL",
            ));
        }
        engine_db("mysql", &db)?;
        if !valid_ident(&table) {
            return Err(ApiError::bad_request("invalid table name"));
        }
        let (cols, pk) = structure_of("mysql", &db, &table).await.map_err(bad)?;
        let names: Vec<String> = cols
            .iter()
            .filter_map(|c| c["name"].as_str().map(String::from))
            .collect();
        for k in b.set.keys().chain(b.pk.keys()) {
            if !names.contains(k) {
                return Err(ApiError::bad_request(format!("unknown column {k}")));
            }
        }
        let sql = match action.as_str() {
            "update" => {
                if b.set.is_empty() {
                    return Err(ApiError::bad_request("nothing to change"));
                }
                let set: Vec<String> = b
                    .set
                    .iter()
                    .map(|(k, v)| format!("{} = {}", qi_my(k), lit_my(v)))
                    .collect();
                format!(
                    "UPDATE {} SET {} WHERE {} LIMIT 1",
                    qi_my(&table),
                    set.join(", "),
                    where_pk(&pk, &b.pk)?
                )
            }
            "delete" => format!(
                "DELETE FROM {} WHERE {} LIMIT 1",
                qi_my(&table),
                where_pk(&pk, &b.pk)?
            ),
            "insert" => {
                if b.set.is_empty() {
                    return Err(ApiError::bad_request("give at least one value"));
                }
                let (ks, vs): (Vec<String>, Vec<String>) =
                    b.set.iter().map(|(k, v)| (qi_my(k), lit_my(v))).unzip();
                format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    qi_my(&table),
                    ks.join(", "),
                    vs.join(", ")
                )
            }
            _ => {
                return Err(ApiError::bad_request(
                    "action must be update, delete or insert",
                ))
            }
        };
        let t = mysql(
            Some(&db),
            &format!("{sql};\nSELECT ROW_COUNT() AS `__affected`;"),
            QUERY_TIMEOUT,
        )
        .await
        .map_err(bad)?;
        let (_, affected) = split_affected(t);
        Ok(Json(json!({"ok": true, "affected": affected})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

// ------------------------------------------------------------------ databases & users (mysql)

fn gen_password() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    Alphanumeric.sample_string(&mut rand::rng(), 24)
}

#[derive(Deserialize)]
pub struct CreateDb {
    name: String,
    /// Create a user with all privileges on the new database.
    user: Option<String>,
}

/// Create a database (and optionally a user owning it). Returns the new
/// user's password. Names must already be validated.
pub async fn create_db_with_user(
    engine: &str,
    name: &str,
    user: Option<&str>,
) -> Result<Option<String>, String> {
    let password = user.map(|_| gen_password());
    if engine == "mysql" {
        let mut sql = format!(
            "CREATE DATABASE {} CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;\n",
            qi_my(name)
        );
        if let (Some(u), Some(p)) = (user, &password) {
            for host in ["localhost", "127.0.0.1"] {
                sql.push_str(&format!(
                    "CREATE USER {u}@{h} IDENTIFIED BY {pw};\nGRANT ALL PRIVILEGES ON {db}.* TO {u}@{h};\n",
                    u = qv_my(u), h = qv_my(host), pw = qv_my(p), db = qi_my(name)
                ));
            }
            sql.push_str("FLUSH PRIVILEGES;\n");
        }
        mysql(None, &sql, QUERY_TIMEOUT).await.map_err(|e| format!("{e:#}"))?;
    } else if engine == "postgres" {
        if let (Some(u), Some(p)) = (user, &password) {
            psql("postgres", &format!("CREATE ROLE {} LOGIN PASSWORD {};", qi_pg(u), qv_pg(p)), QUERY_TIMEOUT).await.map_err(|e| format!("{e:#}"))?;
            psql("postgres", &format!("CREATE DATABASE {} OWNER {};", qi_pg(name), qi_pg(u)), QUERY_TIMEOUT).await.map_err(|e| format!("{e:#}"))?;
        } else {
            psql("postgres", &format!("CREATE DATABASE {};", qi_pg(name)), QUERY_TIMEOUT).await.map_err(|e| format!("{e:#}"))?;
        }
    } else {
        return Err("engine must be mysql or postgres".into());
    }
    Ok(password)
}

pub async fn create_database(
    UrlPath(engine): UrlPath<String>,
    Json(b): Json<CreateDb>,
) -> Response {
    let info = AuditInfo::action("dbm.database.create", format!("{engine}/{}", b.name));
    let res = async {
        engine_db(&engine, &b.name)?;
        if b.name.contains('-') {
            return Err(ApiError::bad_request("use letters, digits and _ only"));
        }
        if let Some(u) = &b.user {
            if !valid_ident(u) || u.contains('-') || u.len() > 32 {
                return Err(ApiError::bad_request("invalid user name"));
            }
        }
        let password = create_db_with_user(&engine, &b.name, b.user.as_deref())
            .await
            .map_err(ApiError::bad_request)?;
        Ok(Json(json!({
            "database": b.name, "user": b.user, "password": password, "host": "127.0.0.1",
            "port": if engine == "mysql" { 3306 } else { 5432 },
            "env": b.user.as_ref().map(|u| format!(
                "DB_CONNECTION={}\nDB_HOST=127.0.0.1\nDB_PORT={}\nDB_DATABASE={}\nDB_USERNAME={u}\nDB_PASSWORD={}",
                if engine == "mysql" { "mysql" } else { "pgsql" }, if engine == "mysql" { 3306 } else { 5432 }, b.name, password.clone().unwrap_or_default()
            )),
        })).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

pub async fn users(UrlPath(engine): UrlPath<String>) -> ApiResult<Json<Value>> {
    if engine == "mysql" {
        let t = mysql(None, "SELECT u.User, u.Host, IFNULL(GROUP_CONCAT(DISTINCT d.Db ORDER BY d.Db SEPARATOR ', '), '') FROM mysql.user u LEFT JOIN mysql.db d ON d.User = u.User AND d.Host = u.Host WHERE u.User NOT IN ('mysql.sys','mysql.session','mysql.infoschema','debian-sys-maint') GROUP BY u.User, u.Host ORDER BY u.User", QUERY_TIMEOUT).await.map_err(bad)?;
        Ok(Json(json!(t
            .rows
            .iter()
            .map(|r| json!({"user": r[0], "host": r[1], "databases": r[2]}))
            .collect::<Vec<_>>())))
    } else if engine == "postgres" {
        let t = psql("postgres", "SELECT rolname, CASE WHEN rolsuper THEN 'superuser' ELSE '' END, (SELECT string_agg(datname, ', ') FROM pg_database WHERE datdba = r.oid) FROM pg_roles r WHERE rolcanlogin ORDER BY 1", QUERY_TIMEOUT).await.map_err(bad)?;
        Ok(Json(json!(t
            .rows
            .iter()
            .map(|r| json!({"user": r[0], "host": r[1], "databases": r[2]}))
            .collect::<Vec<_>>())))
    } else {
        Err(ApiError::bad_request("engine must be mysql or postgres"))
    }
}

#[derive(Deserialize)]
pub struct UserRef {
    user: String,
    #[serde(default = "localhost")]
    host: String,
}
fn localhost() -> String {
    "localhost".into()
}

pub async fn reset_password(UrlPath(engine): UrlPath<String>, Json(b): Json<UserRef>) -> Response {
    let info = AuditInfo::action("dbm.user.password", format!("{engine}/{}", b.user));
    let res = async {
        if !valid_ident(&b.user)
            || b.user == "root"
            || b.user == "postgres"
            || b.user == "debian-sys-maint"
        {
            return Err(ApiError::bad_request(
                "this user's password cannot be changed here",
            ));
        }
        let p = gen_password();
        if engine == "mysql" {
            if !b
                .host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".%_-:".contains(&c))
            {
                return Err(ApiError::bad_request("invalid host"));
            }
            let hosts: Vec<String> = mysql(
                None,
                &format!(
                    "SELECT Host FROM mysql.user WHERE User = {}",
                    qv_my(&b.user)
                ),
                QUERY_TIMEOUT,
            )
            .await
            .map(|t| {
                t.rows
                    .into_iter()
                    .filter_map(|r| r.into_iter().next().flatten())
                    .collect()
            })
            .unwrap_or_default();
            let mut sql = String::new();
            for h in hosts.iter().filter(|h| {
                *h == &b.host || matches!(h.as_str(), "localhost" | "127.0.0.1" | "::1")
            }) {
                sql.push_str(&format!(
                    "ALTER USER {}@{} IDENTIFIED BY {};\n",
                    qv_my(&b.user),
                    qv_my(h),
                    qv_my(&p)
                ));
            }
            if sql.is_empty() {
                return Err(ApiError::not_found("user not found"));
            }
            mysql(None, &sql, QUERY_TIMEOUT).await.map_err(bad)?;
        } else if engine == "postgres" {
            psql(
                "postgres",
                &format!("ALTER ROLE {} PASSWORD {};", qi_pg(&b.user), qv_pg(&p)),
                QUERY_TIMEOUT,
            )
            .await
            .map_err(bad)?;
        } else {
            return Err(ApiError::bad_request("engine must be mysql or postgres"));
        }
        Ok(Json(json!({"user": b.user, "password": p})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(qv_my("it's \\ ok"), "'it\\'s \\\\ ok'");
        assert_eq!(qi_my("we`ird"), "`we``ird`");
        assert_eq!(qv_pg("o'k"), "'o''k'");
        assert!(valid_ident("orders_2026"));
        assert!(!valid_ident("a;drop"));
    }

    #[test]
    fn batch_parsing() {
        let t = parse_batch("id\tname\tnote\n1\tAna\tNULL\n2\tB\\tC\tline\\nbreak\n");
        assert_eq!(t.columns, ["id", "name", "note"]);
        assert_eq!(t.rows[0][2], None);
        assert_eq!(t.rows[1][1].as_deref(), Some("B\tC"));
        assert_eq!(t.rows[1][2].as_deref(), Some("line\nbreak"));
    }

    #[test]
    fn csv_parsing() {
        let t = parse_csv("id,name,note\n1,\"a, b\",\n2,\"say \"\"hi\"\"\",\"\"\n");
        assert_eq!(t.rows[0][1].as_deref(), Some("a, b"));
        assert_eq!(t.rows[0][2], None);
        assert_eq!(t.rows[1][1].as_deref(), Some("say \"hi\""));
        assert_eq!(t.rows[1][2].as_deref(), Some(""));
    }

    #[test]
    fn statement_classification() {
        assert_eq!(classify("SELECT * FROM t"), (true, false));
        assert_eq!(classify("DELETE FROM t"), (true, true));
        assert_eq!(classify("delete from t where id = 1;"), (true, false));
        assert_eq!(classify("DROP TABLE t"), (true, true));
        assert_eq!(classify("SELECT 1; DROP TABLE t"), (false, true));
        assert_eq!(classify("SELECT ';' -- ; drop\n"), (true, false));
        assert_eq!(classify("UPDATE t SET a = 'where'"), (true, true));
    }

    #[test]
    fn pg_guard() {
        assert!(console_guard("postgres", "SELECT * FROM t").is_ok());
        assert!(console_guard("postgres", "SELECT 1 \\! id").is_err());
        assert!(console_guard("postgres", "\\copy t to '/tmp/x'").is_err());
        assert!(console_guard("postgres", "COPY t TO PROGRAM 'id'").is_err());
        assert!(console_guard("postgres", "SELECT 'a\\b'").is_ok());
        // a backslash does not escape the quote in Postgres
        assert!(console_guard(
            "postgres",
            "SELECT '\\'; SET default_transaction_read_only=off; DROP TABLE t"
        )
        .is_err());
        assert!(console_guard("postgres", "SELECT $$a;b$$").is_ok());
        assert!(console_guard("postgres", "SELECT $x$;$x$; DROP TABLE t").is_err());
        assert_eq!(classify_engine("postgres", "DELETE FROM t"), (true, true));
    }

    #[test]
    fn affected_split() {
        let t = parse_batch("id\n1\n2\n__affected\n-1\n");
        let (t, a) = split_affected(t);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(a, Some(-1));
        let t = parse_batch("__affected\n3\n");
        let (t, a) = split_affected(t);
        assert!(t.rows.is_empty());
        assert_eq!(a, Some(3));
    }
}
