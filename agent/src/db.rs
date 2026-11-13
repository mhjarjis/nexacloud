//! SQLite state store (`state_db`, default `/var/lib/nexacloud/state.db`).
//!
//! One connection behind a mutex, WAL journal, embedded migrations tracked in
//! `schema_version`. Every statement the agent runs is short, so callers lock,
//! run their query and release; nothing holds the lock across an `.await`.
//!
//! The CLI (`nexacloud rollback`, `nexacloud doctor`) opens the same file while
//! the service may be running; WAL + `busy_timeout` make that safe.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Ordered migrations. Never edit a shipped entry; append a new one.
/// Index + 1 is the schema version it produces.
const MIGRATIONS: &[&str] = &[
    // v1 — Phase 0 safety foundation.
    r#"
    CREATE TABLE jobs (
        id           TEXT PRIMARY KEY,
        kind         TEXT NOT NULL,
        title        TEXT NOT NULL,
        status       TEXT NOT NULL,
        heavy        INTEGER NOT NULL DEFAULT 0,
        locks        TEXT NOT NULL DEFAULT '[]',
        timeout_secs INTEGER,
        cancellable  INTEGER NOT NULL DEFAULT 1,
        created_at   TEXT NOT NULL,
        started_at   TEXT,
        finished_at  TEXT,
        error        TEXT,
        change_id    TEXT,
        actor_ip     TEXT
    );
    CREATE INDEX jobs_created ON jobs (created_at);

    CREATE TABLE changes (
        id           TEXT PRIMARY KEY,
        kind         TEXT NOT NULL,
        title        TEXT NOT NULL,
        target       TEXT NOT NULL,
        status       TEXT NOT NULL,
        plan         TEXT NOT NULL,
        plan_hash    TEXT NOT NULL,
        job_id       TEXT,
        actor_ip     TEXT,
        created_at   TEXT NOT NULL,
        finished_at  TEXT,
        error        TEXT,
        verify       TEXT NOT NULL DEFAULT '[]',
        rollback_of  TEXT,
        rolled_back_by TEXT
    );
    CREATE INDEX changes_created ON changes (created_at);

    CREATE TABLE audit_log (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        ts         TEXT NOT NULL,
        ip         TEXT,
        action     TEXT NOT NULL,
        target     TEXT,
        change_id  TEXT,
        job_id     TEXT,
        plan_hash  TEXT,
        result     TEXT NOT NULL
    );
    CREATE TRIGGER audit_log_no_update BEFORE UPDATE ON audit_log
        BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END;
    CREATE TRIGGER audit_log_no_delete BEFORE DELETE ON audit_log
        BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END;

    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    "#,
    // v2 — Phase 1: backups.
    r#"
    CREATE TABLE backup_destinations (
        id         TEXT PRIMARY KEY,
        name       TEXT NOT NULL,
        kind       TEXT NOT NULL,
        config     TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    CREATE TABLE backup_plans (
        id             TEXT PRIMARY KEY,
        name           TEXT NOT NULL,
        destination_id TEXT,
        databases      TEXT NOT NULL DEFAULT '[]',
        paths          TEXT NOT NULL DEFAULT '[]',
        schedule       TEXT NOT NULL,
        keep_daily     INTEGER NOT NULL DEFAULT 7,
        keep_weekly    INTEGER NOT NULL DEFAULT 4,
        keep_monthly   INTEGER NOT NULL DEFAULT 3,
        enabled        INTEGER NOT NULL DEFAULT 1,
        last_run_at    TEXT,
        created_at     TEXT NOT NULL
    );
    CREATE TABLE backup_runs (
        id          TEXT PRIMARY KEY,
        plan_id     TEXT NOT NULL,
        job_id      TEXT,
        status      TEXT NOT NULL,
        stamp       TEXT NOT NULL,
        started_at  TEXT NOT NULL,
        finished_at TEXT,
        size_bytes  INTEGER NOT NULL DEFAULT 0,
        items       TEXT NOT NULL DEFAULT '[]',
        local_path  TEXT,
        remote_path TEXT,
        error       TEXT
    );
    CREATE INDEX backup_runs_plan ON backup_runs (plan_id, started_at);
    "#,
    // v3 — Phase 2: projects and deploys.
    r#"
    CREATE TABLE projects (
        id             TEXT PRIMARY KEY,
        site_id        TEXT NOT NULL,
        kind           TEXT NOT NULL,
        dir            TEXT NOT NULL,
        repo           TEXT NOT NULL,
        branch         TEXT NOT NULL,
        run_as         TEXT NOT NULL,
        script         TEXT NOT NULL,
        migrate        INTEGER NOT NULL DEFAULT 1,
        auto_deploy    INTEGER NOT NULL DEFAULT 0,
        webhook_secret TEXT NOT NULL,
        created_at     TEXT NOT NULL,
        last_deploy_at TEXT,
        last_commit    TEXT
    );
    CREATE TABLE deploys (
        id              TEXT PRIMARY KEY,
        project_id      TEXT NOT NULL,
        job_id          TEXT,
        status          TEXT NOT NULL,
        trigger         TEXT NOT NULL,
        commit_sha      TEXT,
        message         TEXT,
        author          TEXT,
        previous_commit TEXT,
        started_at      TEXT NOT NULL,
        finished_at     TEXT,
        error           TEXT
    );
    CREATE INDEX deploys_project ON deploys (project_id, started_at);
    "#,
];

/// Latest schema version this binary knows.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Shared handle to the state database.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// Open (creating if needed) the database file, enable WAL and run any
    /// pending migrations. The file is created with mode 0600.
    pub fn open(path: &Path) -> Result<Db> {
        let existed = path.exists();
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        if !existed {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("enabling WAL")?;
        Self::setup(conn)
    }

    /// In-memory database (tests).
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Db> {
        Self::setup(Connection::open_in_memory()?)
    }

    fn setup(conn: Connection) -> Result<Db> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&conn)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Lock the connection. Keep the guard short-lived and never hold it
    /// across an `.await`.
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Current schema version recorded in the file.
    pub fn schema_version(&self) -> Result<i64> {
        current_version(&self.conn())
    }

    /// `PRAGMA integrity_check` — `Ok(())` when SQLite reports "ok".
    pub fn integrity_check(&self) -> Result<(), String> {
        let c = self.conn();
        let res: rusqlite::Result<String> = c.query_row("PRAGMA integrity_check", [], |r| r.get(0));
        match res {
            Ok(s) if s == "ok" => Ok(()),
            Ok(s) => Err(s),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Read a value from the `settings` table.
    #[allow(dead_code)] // used by later phases (scheduler, advisor snoozes)
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Insert or replace a value in the `settings` table.
    #[allow(dead_code)] // used by later phases (scheduler, advisor snoozes)
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }
}

fn current_version(conn: &Connection) -> Result<i64> {
    let has_table: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_version'",
        [],
        |r| r.get::<_, i64>(0).map(|n| n > 0),
    )?;
    if !has_table {
        return Ok(0);
    }
    Ok(conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| {
            r.get::<_, Option<i64>>(0)
        })?
        .unwrap_or(0))
}

/// Apply every migration newer than the recorded version, each in its own
/// transaction. Refuses to run against a database from a newer agent.
fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)")?;
    let current = current_version(conn)?;
    if current > SCHEMA_VERSION {
        anyhow::bail!(
            "state database schema v{current} is newer than this agent (v{SCHEMA_VERSION}); upgrade nexacloud"
        );
    }
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = i as i64 + 1;
        if version <= current {
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)
            .with_context(|| format!("migration v{version}"))?;
        tx.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [version],
        )?;
        tx.commit()?;
        tracing::info!("state database migrated to v{version}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("nexacloud-db-{}.db", uuid::Uuid::new_v4()))
    }

    #[test]
    fn migrates_once_and_is_idempotent() {
        let p = tmp_path();
        {
            let db = Db::open(&p).unwrap();
            assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
            for t in ["jobs", "changes", "audit_log", "settings"] {
                let n: i64 = db
                    .conn()
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                        [t],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(n, 1, "table {t}");
            }
            db.set_setting("k", "v1").unwrap();
            db.set_setting("k", "v2").unwrap();
        }
        // Re-open: no migration re-run (would fail on CREATE TABLE), data kept.
        let db = Db::open(&p).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let rows: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, SCHEMA_VERSION);
        assert_eq!(db.setting("k").unwrap().as_deref(), Some("v2"));
        assert!(db.integrity_check().is_ok());
        let mode: String = db
            .conn()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(db);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", p.display()));
        }
    }

    #[test]
    fn refuses_newer_schema() {
        let db = Db::open_in_memory().unwrap();
        db.conn()
            .execute("INSERT INTO schema_version (version) VALUES (999)", [])
            .unwrap();
        let err = migrate(&db.conn()).unwrap_err().to_string();
        assert!(err.contains("newer"), "{err}");
    }

    #[test]
    fn audit_log_is_append_only() {
        let db = Db::open_in_memory().unwrap();
        let c = db.conn();
        c.execute(
            "INSERT INTO audit_log (ts, action, result) VALUES ('t', 'a', 'ok')",
            [],
        )
        .unwrap();
        assert!(c.execute("UPDATE audit_log SET result='x'", []).is_err());
        assert!(c.execute("DELETE FROM audit_log", []).is_err());
    }
}
