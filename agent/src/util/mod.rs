pub mod cmd;

use std::future::Future;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};

/// RFC 3339, UTC, whole seconds, `Z` suffix.
pub fn fmt_ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Run futures concurrently on the runtime and return their outputs in input
/// order. A task that panicked yields `None` (and is logged) instead of
/// taking the request down with it.
pub async fn join_all<F>(futs: Vec<F>) -> Vec<Option<F::Output>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut set = tokio::task::JoinSet::new();
    let n = futs.len();
    for (i, f) in futs.into_iter().enumerate() {
        set.spawn(async move { (i, f.await) });
    }
    let mut out: Vec<Option<F::Output>> = (0..n).map(|_| None).collect();
    while let Some(res) = set.join_next().await {
        match res {
            Ok((i, v)) => out[i] = Some(v),
            Err(e) => tracing::error!("background task failed: {e}"),
        }
    }
    out
}

/// `^[A-Za-z0-9_\-]{1,64}$`
pub fn is_valid_db_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Create a directory (and parents) with mode 0700 and force 0700 on the leaf.
pub fn create_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// Atomically write a file with mode 0600 (write temp file, then rename).
pub fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_names() {
        assert!(is_valid_db_name("shop_prod"));
        assert!(is_valid_db_name("my-db-1"));
        assert!(is_valid_db_name(&"a".repeat(64)));
        assert!(!is_valid_db_name(&"a".repeat(65)));
        assert!(!is_valid_db_name(""));
        assert!(!is_valid_db_name("shop prod"));
        assert!(!is_valid_db_name("../etc"));
        assert!(!is_valid_db_name("a;drop"));
        assert!(!is_valid_db_name("host=evil"));
        assert!(!is_valid_db_name("dé"));
    }

    #[test]
    fn ts_format() {
        let dt = DateTime::parse_from_rfc3339("2026-11-13T03:00:00.123Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(fmt_ts(dt), "2026-11-13T03:00:00Z");
    }
}
