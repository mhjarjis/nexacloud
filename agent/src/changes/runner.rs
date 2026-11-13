//! How the change pipeline touches the outside world: an injectable
//! filesystem root and command runner, so tests never touch the real system.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::util::cmd::{Cmd, CmdOutput, LineSink};

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Runs plan commands and HTTP probes.
pub trait Runner: Send + Sync {
    /// Run `argv` (never through a shell), streaming output lines to `log`.
    fn run<'a>(
        &'a self,
        argv: &'a [String],
        timeout: Duration,
        log: &'a dyn LineSink,
    ) -> BoxFut<'a, CmdOutput>;

    /// HTTP status of a GET to `url` (TLS errors ignored), or why it failed.
    fn http_status<'a>(
        &'a self,
        url: &'a str,
        timeout: Duration,
    ) -> BoxFut<'a, Result<u16, String>>;
}

/// The real system: commands via [`Cmd::run_streaming`] (own process group,
/// killed on timeout), HTTP probes via `curl`.
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run<'a>(
        &'a self,
        argv: &'a [String],
        timeout: Duration,
        log: &'a dyn LineSink,
    ) -> BoxFut<'a, CmdOutput> {
        Box::pin(async move {
            let Some((prog, args)) = argv.split_first() else {
                return CmdOutput {
                    stderr: "empty command".into(),
                    ..Default::default()
                };
            };
            let mut c = Cmd::new(prog);
            if prog == "apt-get" {
                // Never prompt, and wait (up to 5 min) for unattended-upgrades
                // to release the dpkg lock instead of failing at once.
                c = c
                    .env("DEBIAN_FRONTEND", "noninteractive")
                    .args(["-o", "DPkg::Lock::Timeout=300"]);
            }
            c.args(args).timeout(timeout).run_streaming(log).await
        })
    }

    fn http_status<'a>(
        &'a self,
        url: &'a str,
        timeout: Duration,
    ) -> BoxFut<'a, Result<u16, String>> {
        Box::pin(async move {
            let out = Cmd::new("curl")
                .args(["-sS", "-k", "-o", "/dev/null", "-w", "%{http_code}"])
                .arg("--max-time")
                .arg(timeout.as_secs().max(1).to_string())
                .arg("--url")
                .arg(url)
                .timeout(timeout + Duration::from_secs(5))
                .run()
                .await;
            let code = out.stdout.trim().parse::<u16>().unwrap_or(0);
            if code == 0 {
                Err(out.error_summary("curl"))
            } else {
                Ok(code)
            }
        })
    }
}

/// Where a change pipeline operates.
#[derive(Clone)]
pub struct ChangeEnv {
    /// Filesystem root that plan paths are relative to (`/` in production,
    /// a temp dir in tests).
    pub root: PathBuf,
    /// `{data_dir}/snapshots`.
    pub snapshots_dir: PathBuf,
    pub runner: Arc<dyn Runner>,
}

impl ChangeEnv {
    /// Production environment: real `/`, real commands.
    pub fn system(snapshots_dir: PathBuf) -> Self {
        ChangeEnv {
            root: PathBuf::from("/"),
            snapshots_dir,
            runner: Arc::new(SystemRunner),
        }
    }

    /// Map an absolute plan path (`/etc/nginx/x.conf`) onto `root`.
    pub fn host_path(&self, plan_path: &str) -> PathBuf {
        let rel = plan_path.trim_start_matches('/');
        self.root.join(rel)
    }
}

/// Plan paths must be absolute, normalised (no `.`/`..`), and NUL-free.
pub fn validate_plan_path(p: &str) -> Result<(), String> {
    if p.contains('\0') {
        return Err(format!("invalid path {p:?}"));
    }
    let path = Path::new(p);
    if !p.starts_with('/') {
        return Err(format!("path must be absolute: {p}"));
    }
    // Check the raw segments: `Path::components` silently drops `.` and `//`.
    if p[1..]
        .split('/')
        .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err(format!("path must be normalised (no ., .., //): {p}"));
    }
    if path.parent().is_none() || path.file_name().is_none() {
        return Err(format!("not a file path: {p}"));
    }
    Ok(())
}

#[cfg(test)]
pub mod fake {
    //! Scripted runner for tests.
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Every command succeeds unless scripted to fail; records each call.
    #[derive(Default)]
    pub struct FakeRunner {
        /// argv joined with spaces → (exit code, stdout).
        pub scripted: Mutex<HashMap<String, (i32, String)>>,
        pub http: Mutex<HashMap<String, u16>>,
        pub calls: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        pub fn script(&self, cmd: &str, code: i32, stdout: &str) {
            self.scripted
                .lock()
                .unwrap()
                .insert(cmd.to_string(), (code, stdout.to_string()));
        }
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Runner for FakeRunner {
        fn run<'a>(
            &'a self,
            argv: &'a [String],
            _timeout: Duration,
            log: &'a dyn LineSink,
        ) -> BoxFut<'a, CmdOutput> {
            Box::pin(async move {
                let key = argv.join(" ");
                self.calls.lock().unwrap().push(key.clone());
                let (code, stdout) = self
                    .scripted
                    .lock()
                    .unwrap()
                    .get(&key)
                    .cloned()
                    .unwrap_or((0, String::new()));
                for l in stdout.lines() {
                    log.line(l);
                }
                CmdOutput {
                    ok: code == 0,
                    exit_code: Some(code),
                    stdout,
                    stderr: if code == 0 {
                        String::new()
                    } else {
                        format!("{key}: scripted failure")
                    },
                    ..Default::default()
                }
            })
        }

        fn http_status<'a>(
            &'a self,
            url: &'a str,
            _timeout: Duration,
        ) -> BoxFut<'a, Result<u16, String>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("GET {url}"));
                self.http
                    .lock()
                    .unwrap()
                    .get(url)
                    .copied()
                    .ok_or_else(|| "connection refused".to_string())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_paths() {
        assert!(validate_plan_path("/etc/nginx/conf.d/x.conf").is_ok());
        assert!(validate_plan_path("etc/x").is_err());
        assert!(validate_plan_path("/etc/../x").is_err());
        assert!(validate_plan_path("/etc/./x").is_err());
        assert!(validate_plan_path("/").is_err());
        assert!(validate_plan_path("/etc/x\0").is_err());
        assert!(validate_plan_path("/etc//x").is_err());
        assert!(validate_plan_path("/etc/x/").is_err());
        let env = ChangeEnv {
            root: PathBuf::from("/tmp/root"),
            snapshots_dir: PathBuf::from("/tmp/snaps"),
            runner: Arc::new(SystemRunner),
        };
        assert_eq!(
            env.host_path("/etc/nginx/a.conf"),
            PathBuf::from("/tmp/root/etc/nginx/a.conf")
        );
    }
}
