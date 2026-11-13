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

/// Lowercase hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

/// Atomically replace `path` with `contents`: write a temp file in the same
/// directory, fsync it, set `mode` (and `owner` uid/gid if given), rename it
/// over the target and fsync the directory. Readers see either the old or the
/// new file, never a partial one. The temp name never ends in `.conf`, so
/// globbed includes (`conf.d/*.conf`) can't pick it up half-written.
pub fn atomic_write(
    path: &Path,
    contents: &[u8],
    mode: u32,
    owner: Option<(u32, u32)>,
) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let tmp = dir.join(format!(
        ".{name}.nc-tmp-{}-{}",
        std::process::id(),
        rand::random::<u32>()
    ));
    let result = (|| {
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(contents)?;
            f.sync_all()?;
        }
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        if let Some((uid, gid)) = owner {
            std::os::unix::fs::chown(&tmp, Some(uid), Some(gid))?;
        }
        std::fs::rename(&tmp, path)?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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
    fn atomic_write_sets_mode_and_replaces() {
        let dir = std::env::temp_dir().join(format!("nc-aw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.conf");
        atomic_write(&p, b"one", 0o644, None).unwrap();
        atomic_write(&p, b"two", 0o640, None).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        // No temp files left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ts_format() {
        let dt = DateTime::parse_from_rfc3339("2026-11-13T03:00:00.123Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(fmt_ts(dt), "2026-11-13T03:00:00Z");
    }
}
