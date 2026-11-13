//! Running external commands safely: argument arrays only (never a shell),
//! stdin closed, a hard timeout, and the child killed if we stop waiting.

use std::io::ErrorKind;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// Default timeout for external commands.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default)]
pub struct CmdOutput {
    /// Process ran and exited with status 0.
    pub ok: bool,
    /// The binary could not be found / spawned.
    pub missing: bool,
    /// We gave up waiting and killed it.
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOutput {
    /// stdout and stderr joined (some tools print versions to stderr).
    pub fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }

    /// A short, single-line description of why the command failed.
    pub fn error_summary(&self, program: &str) -> String {
        if self.missing {
            return format!("{program} is not installed");
        }
        if self.timed_out {
            return format!("{program} timed out");
        }
        let msg = self.stderr.trim();
        let msg = if msg.is_empty() {
            self.stdout.trim()
        } else {
            msg
        };
        let msg: String = msg.lines().take(3).collect::<Vec<_>>().join(" ");
        if msg.is_empty() {
            format!("{program} failed")
        } else {
            format!("{program} failed: {}", truncate(&msg, 300))
        }
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Builder for a single external command invocation.
pub struct Cmd {
    program: String,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    timeout: Duration,
}

impl Cmd {
    pub fn new(program: &str) -> Self {
        Cmd {
            program: program.to_string(),
            args: Vec::new(),
            envs: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn arg(mut self, a: impl AsRef<str>) -> Self {
        self.args.push(a.as_ref().to_string());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_string()));
        self
    }

    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.envs.push((k.to_string(), v.to_string()));
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    /// Build the underlying tokio command (used by pipelines that need
    /// custom stdio). Always: no shell, LC_ALL=C, stdin closed, kill on drop.
    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args)
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for (k, v) in &self.envs {
            c.env(k, v);
        }
        c
    }

    pub async fn run(self) -> CmdOutput {
        let mut c = self.command();
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = match c.spawn() {
            Ok(ch) => ch,
            Err(e) => {
                return CmdOutput {
                    missing: e.kind() == ErrorKind::NotFound
                        || e.kind() == ErrorKind::PermissionDenied,
                    stderr: e.to_string(),
                    ..Default::default()
                }
            }
        };
        // If the timeout fires, the future (and with it the child) is dropped,
        // and kill_on_drop terminates the process.
        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(out)) => CmdOutput {
                ok: out.status.success(),
                missing: false,
                timed_out: false,
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            },
            Ok(Err(e)) => CmdOutput {
                stderr: e.to_string(),
                ..Default::default()
            },
            Err(_) => CmdOutput {
                timed_out: true,
                stderr: format!("timed out after {}s", self.timeout.as_secs()),
                ..Default::default()
            },
        }
    }
}

/// Extract the first version-looking token (`\d+(\.\d+)+`) from text.
pub fn extract_version(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let tok = text[start..i].trim_end_matches('.');
            if tok.contains('.') && !tok.contains("..") {
                return Some(tok.to_string());
            }
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(
            extract_version("nginx version: nginx/1.24.0 (Ubuntu)").as_deref(),
            Some("1.24.0")
        );
        assert_eq!(
            extract_version("/usr/sbin/mysqld  Ver 8.0.40-0ubuntu0.24.04.1 for Linux").as_deref(),
            Some("8.0.40")
        );
        assert_eq!(
            extract_version("Redis server v=7.0.15 sha=00000000:0").as_deref(),
            Some("7.0.15")
        );
        assert_eq!(
            extract_version("psql (PostgreSQL) 16.4 (Ubuntu 16.4-0ubuntu0.24.04.2)").as_deref(),
            Some("16.4")
        );
        assert_eq!(extract_version("Fail2Ban v1.0.2").as_deref(), Some("1.0.2"));
        assert_eq!(extract_version("no digits"), None);
        assert_eq!(extract_version("only 5 here"), None);
    }

    #[tokio::test]
    async fn missing_binary_does_not_error() {
        let out = Cmd::new("definitely-not-a-real-binary-nexacloud")
            .run()
            .await;
        assert!(out.missing);
        assert!(!out.ok);
    }

    #[tokio::test]
    async fn timeout_kills() {
        let out = Cmd::new("sleep")
            .arg("5")
            .timeout(Duration::from_millis(200))
            .run()
            .await;
        assert!(out.timed_out);
        assert!(!out.ok);
    }
}
