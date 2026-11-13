//! `/etc/nexacloud/config.toml` loading, creation (`init`) and password reset.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::auth;
use crate::util::{create_private_dir, write_private_file};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/nexacloud/config.toml";
pub const DEFAULT_LISTEN: &str = "0.0.0.0:8443";
pub const DEFAULT_WEB_DIR: &str = "/opt/nexacloud/web";
pub const DEFAULT_BACKUP_DIR: &str = "/var/backups/nexacloud";
/// Agent state: SQLite database, job logs, change snapshots.
pub const DEFAULT_DATA_DIR: &str = "/var/lib/nexacloud";

fn default_true() -> bool {
    true
}

/// The config file as written on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_web_dir")]
    pub web_dir: PathBuf,
    #[serde(default = "default_backup_dir")]
    pub backup_dir: PathBuf,
    pub admin_password_hash: String,
    #[serde(default = "default_true")]
    pub tls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_cert: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_key: Option<PathBuf>,
    /// Directory for agent state (`jobs/`, `snapshots/`, and by default the
    /// state database). Created with mode 0700. Added in v0.2; older configs
    /// without it use the default.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// SQLite state database. Defaults to `{data_dir}/state.db` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_db: Option<PathBuf>,
}

fn default_listen() -> String {
    DEFAULT_LISTEN.into()
}
fn default_web_dir() -> PathBuf {
    DEFAULT_WEB_DIR.into()
}
fn default_backup_dir() -> PathBuf {
    DEFAULT_BACKUP_DIR.into()
}
fn default_data_dir() -> PathBuf {
    DEFAULT_DATA_DIR.into()
}

impl Config {
    /// Load and validate the config. Relative paths are resolved against the
    /// directory containing the config file.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        let base = absolute(path)?
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"));
        cfg.web_dir = resolve(&base, &cfg.web_dir);
        cfg.backup_dir = resolve(&base, &cfg.backup_dir);
        cfg.data_dir = resolve(&base, &cfg.data_dir);
        let state_db = cfg
            .state_db
            .as_ref()
            .map(|p| resolve(&base, p))
            .unwrap_or_else(|| cfg.data_dir.join("state.db"));
        cfg.state_db = Some(state_db);
        cfg.tls_cert = cfg.tls_cert.as_ref().map(|p| resolve(&base, p));
        cfg.tls_key = cfg.tls_key.as_ref().map(|p| resolve(&base, p));
        if !auth::is_valid_hash(&cfg.admin_password_hash) {
            bail!(
                "admin_password_hash in {} is not a valid argon2 hash",
                path.display()
            );
        }
        if cfg.tls && (cfg.tls_cert.is_none() || cfg.tls_key.is_none()) {
            bail!("tls = true requires tls_cert and tls_key");
        }
        Ok(cfg)
    }

    /// Path of the SQLite state database (always set after `load`).
    pub fn state_db_path(&self) -> PathBuf {
        self.state_db
            .clone()
            .unwrap_or_else(|| self.data_dir.join("state.db"))
    }

    /// `{data_dir}/jobs`: one `<job id>.log` per job.
    pub fn jobs_dir(&self) -> PathBuf {
        self.data_dir.join("jobs")
    }

    /// `{data_dir}/snapshots`: one `<change id>/` per applied change.
    pub fn snapshots_dir(&self) -> PathBuf {
        self.data_dir.join("snapshots")
    }

    /// Create `data_dir` (0700) and its `jobs/` and `snapshots/` subdirs.
    pub fn ensure_data_dirs(&self) -> Result<()> {
        for d in [self.data_dir.clone(), self.jobs_dir(), self.snapshots_dir()] {
            create_private_dir(&d).with_context(|| format!("creating {}", d.display()))?;
        }
        if let Some(parent) = self.state_db_path().parent() {
            if !parent.exists() {
                create_private_dir(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        Ok(())
    }
}

fn absolute(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(p))
    }
}

fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

pub enum InitOutcome {
    Created {
        password: String,
        warnings: Vec<String>,
    },
    Exists,
}

/// `nexacloud init`: create config, TLS cert/key and backup dir if the config
/// does not exist yet. Never touches anything when the config exists.
pub fn init(config_path: &Path) -> Result<InitOutcome> {
    init_with_dirs(
        config_path,
        Path::new(DEFAULT_BACKUP_DIR),
        Path::new(DEFAULT_DATA_DIR),
    )
}

fn init_with_dirs(config_path: &Path, backup_dir: &Path, data_dir: &Path) -> Result<InitOutcome> {
    if config_path.exists() {
        return Ok(InitOutcome::Exists);
    }
    let config_path = absolute(config_path)?;
    let dir = config_path
        .parent()
        .context("config path has no parent directory")?
        .to_path_buf();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let mut warnings = Vec::new();

    let tls_dir = dir.join("tls");
    let cert_path = tls_dir.join("cert.pem");
    let key_path = tls_dir.join("key.pem");
    if !(cert_path.exists() && key_path.exists()) {
        create_private_dir(&tls_dir).with_context(|| format!("creating {}", tls_dir.display()))?;
        let (cert_pem, key_pem) = generate_self_signed()?;
        write_private_file(&key_path, key_pem.as_bytes())
            .with_context(|| format!("writing {}", key_path.display()))?;
        write_private_file(&cert_path, cert_pem.as_bytes())
            .with_context(|| format!("writing {}", cert_path.display()))?;
    }

    let backup_dir = backup_dir.to_path_buf();
    if let Err(e) = create_private_dir(&backup_dir) {
        warnings.push(format!(
            "could not create backup_dir {}: {e} (it will be created on first backup)",
            backup_dir.display()
        ));
    }

    let data_dir = data_dir.to_path_buf();
    if let Err(e) = create_private_dir(&data_dir) {
        warnings.push(format!(
            "could not create data_dir {}: {e} (it will be created when the agent starts)",
            data_dir.display()
        ));
    }

    let password = auth::generate_password();
    let cfg = Config {
        listen: DEFAULT_LISTEN.into(),
        web_dir: DEFAULT_WEB_DIR.into(),
        backup_dir,
        admin_password_hash: auth::hash_password(&password)?,
        tls: true,
        tls_cert: Some(cert_path),
        tls_key: Some(key_path),
        state_db: Some(data_dir.join("state.db")),
        data_dir,
    };
    let text = toml::to_string(&cfg).context("serialising config")?;
    // Written last, so a failure above leaves no config and `init` can be re-run.
    write_private_file(&config_path, text.as_bytes())
        .with_context(|| format!("writing {}", config_path.display()))?;
    Ok(InitOutcome::Created { password, warnings })
}

/// `nexacloud reset-password`: replace only the `admin_password_hash` line,
/// keeping the rest of the file (including comments) intact.
pub fn reset_password(config_path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(config_path)
        .with_context(|| format!("reading config {}", config_path.display()))?;
    // Make sure the file is valid before we touch it.
    let _: toml::Table = toml::from_str(&text)
        .with_context(|| format!("parsing config {}", config_path.display()))?;
    let password = auth::generate_password();
    let hash = auth::hash_password(&password)?;
    let new_text = replace_hash_line(&text, &hash);
    write_private_file(config_path, new_text.as_bytes())
        .with_context(|| format!("writing {}", config_path.display()))?;
    Ok(password)
}

pub fn replace_hash_line(text: &str, hash: &str) -> String {
    let line = format!(
        "admin_password_hash = {}",
        toml::Value::String(hash.to_string())
    );
    let mut out = Vec::new();
    let mut replaced = false;
    for l in text.lines() {
        let t = l.trim_start();
        let is_key = t
            .strip_prefix("admin_password_hash")
            .map(|rest| rest.trim_start().starts_with('='))
            .unwrap_or(false);
        if is_key && !replaced {
            out.push(line.clone());
            replaced = true;
        } else if !is_key {
            out.push(l.to_string());
        }
    }
    if !replaced {
        // Top-level keys must come before any [table]; insert at the top.
        out.insert(0, line);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

fn generate_self_signed() -> Result<(String, String)> {
    let mut names = vec!["localhost".to_string()];
    if let Ok(h) = sysinfo::System::host_name() {
        if !h.is_empty() && h != "localhost" {
            names.insert(0, h);
        }
    }
    let key = rcgen::KeyPair::generate().context("generating TLS key")?;
    let mut params = rcgen::CertificateParams::new(names).context("certificate params")?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "NexaCloud agent");
    let now = chrono::Utc::now();
    use chrono::Datelike;
    params.not_before = rcgen::date_time_ymd(now.year() - 1, 1, 1);
    params.not_after = rcgen::date_time_ymd(now.year() + 10, 1, 1);
    let cert = params
        .self_signed(&key)
        .context("self-signing certificate")?;
    Ok((cert.pem(), key.serialize_pem()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_hash_line_only() {
        let text =
            "# comment\nlisten = \"0.0.0.0:8443\"\nadmin_password_hash = \"old\"\ntls = true\n";
        let out = replace_hash_line(text, "$argon2id$v=19$new");
        assert!(out.contains("# comment"));
        assert!(out.contains("admin_password_hash = \"$argon2id$v=19$new\""));
        assert!(!out.contains("\"old\""));
        let t: toml::Table = toml::from_str(&out).unwrap();
        assert_eq!(
            t["admin_password_hash"].as_str(),
            Some("$argon2id$v=19$new")
        );
        assert_eq!(t["tls"].as_bool(), Some(true));
    }

    #[test]
    fn dev_config_loads_and_password_is_dev() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("dev-config.toml");
        let cfg = Config::load(&path).unwrap();
        assert!(!cfg.tls);
        assert_eq!(cfg.listen, "127.0.0.1:8443");
        assert!(cfg.backup_dir.is_absolute());
        assert!(cfg.data_dir.is_absolute());
        assert!(cfg.state_db_path().ends_with("dev-state.db"));
        assert!(auth::verify_password("dev", &cfg.admin_password_hash));
        assert!(!auth::verify_password("nope", &cfg.admin_password_hash));
    }

    #[test]
    fn old_config_without_new_keys_loads() {
        let dir = std::env::temp_dir().join(format!("nexacloud-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let hash = auth::hash_password("x").unwrap();
        std::fs::write(
            &path,
            format!("admin_password_hash = {:?}\ntls = false\n", hash),
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.data_dir, PathBuf::from(DEFAULT_DATA_DIR));
        assert_eq!(
            cfg.state_db_path(),
            PathBuf::from("/var/lib/nexacloud/state.db")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_then_keep() {
        let dir = std::env::temp_dir().join(format!("nexacloud-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        match init_with_dirs(&path, &dir.join("backups"), &dir.join("data")).unwrap() {
            InitOutcome::Created { password, .. } => {
                assert_eq!(password.len(), 20);
                let cfg = Config::load(&path).unwrap();
                assert!(auth::verify_password(&password, &cfg.admin_password_hash));
                assert!(cfg.tls);
                assert!(dir.join("backups").is_dir());
                assert!(dir.join("data").is_dir());
                assert_eq!(cfg.state_db_path(), dir.join("data").join("state.db"));
                use std::os::unix::fs::PermissionsExt;
                for p in [
                    &path,
                    cfg.tls_key.as_ref().unwrap(),
                    cfg.tls_cert.as_ref().unwrap(),
                ] {
                    let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
                    assert_eq!(mode, 0o600, "{}", p.display());
                }
                let new_pw = reset_password(&path).unwrap();
                let cfg2 = Config::load(&path).unwrap();
                assert!(auth::verify_password(&new_pw, &cfg2.admin_password_hash));
                assert!(!auth::verify_password(&password, &cfg2.admin_password_hash));
            }
            InitOutcome::Exists => panic!("should create"),
        }
        assert!(matches!(init(&path).unwrap(), InitOutcome::Exists));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
