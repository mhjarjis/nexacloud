//! NexaCloud agent: manages a VPS hosting Laravel and plain-PHP sites.

mod advisor;
mod apps;
mod alerts;
mod audit;
mod auth;
mod backups;
mod changes;
mod cli;
mod config;
mod cron;
mod db;
mod dbm;
mod deploys;
mod error;
mod files;
mod jobs;
mod monitor;
mod nginx;
mod plans;
mod routes;
mod state;
mod terminal;
mod users;
mod util;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::config::{Config, InitOutcome, DEFAULT_CONFIG_PATH};
use crate::state::AppState;

#[derive(Parser)]
#[command(name = "nexacloud", version, about = "NexaCloud server agent")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the agent (API + web UI).
    Serve {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
    },
    /// Create config, admin password and self-signed TLS cert if missing.
    Init {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
    },
    /// Generate a new admin password.
    ResetPassword {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
    },
    /// Undo an applied change by restoring its snapshot (works with the
    /// service stopped).
    Rollback {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
        /// Roll back the most recent applied change.
        #[arg(long, conflicts_with = "change_id")]
        last: bool,
        /// Overwrite files that were edited after the change was applied.
        #[arg(long)]
        force: bool,
        /// Change id (see the Activity page).
        change_id: Option<String>,
    },
    /// Check config permissions, state database, nginx, disk space and the
    /// service; exits 1 when something is wrong.
    Doctor {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Serve { config } => serve(config),
        Command::Init { config } => init(config),
        Command::ResetPassword { config } => reset_password(config),
        Command::Rollback {
            config,
            last,
            force,
            change_id,
        } => cli::rollback(&config, change_id, last, force),
        Command::Doctor { config } => return cli::doctor(&config),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn init(path: PathBuf) -> Result<()> {
    match config::init(&path)? {
        InitOutcome::Exists => println!("config exists, keeping it"),
        InitOutcome::Created { password, warnings } => {
            for w in warnings {
                eprintln!("warning: {w}");
            }
            println!("NEXACLOUD_ADMIN_PASSWORD={password}");
        }
    }
    Ok(())
}

fn reset_password(path: PathBuf) -> Result<()> {
    let password = config::reset_password(&path)?;
    println!("NEXACLOUD_ADMIN_PASSWORD={password}");
    // The running agent holds the old hash (and old sessions) in memory.
    // Restart it so the new password works now and existing logins end.
    // `try-restart` does nothing when the service isn't running.
    match std::process::Command::new("systemctl")
        .args(["try-restart", "nexacloud"])
        .status()
    {
        Ok(s) if s.success() => {
            eprintln!("Restarted the nexacloud service: the new password works now and old sessions were signed out.")
        }
        _ => eprintln!(
            "Note: could not restart the nexacloud service. Run `systemctl restart nexacloud` for the new password to take effect."
        ),
    }
    Ok(())
}

fn serve(path: PathBuf) -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .init();
    let cfg = Config::load(&path)?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting runtime")?
        .block_on(run(cfg))
}

async fn run(cfg: Config) -> Result<()> {
    let addr: SocketAddr = cfg
        .listen
        .parse()
        .with_context(|| format!("invalid listen address {:?}", cfg.listen))?;
    let tls = if cfg.tls {
        // We build with rustls' `ring` backend only; install it explicitly.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = cfg.tls_cert.clone().context("tls_cert missing")?;
        let key = cfg.tls_key.clone().context("tls_key missing")?;
        Some(
            axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                .await
                .with_context(|| {
                    format!(
                        "loading TLS cert {} / key {}",
                        cert.display(),
                        key.display()
                    )
                })?,
        )
    } else {
        None
    };

    let state = AppState::new(cfg)?;
    // Startup sweep (serve only — the CLI opens the same database while the
    // service may be running): work owned by a previous process will never
    // finish.
    let jobs = state.jobs.mark_interrupted_at_startup()?;
    let chg = changes::mark_interrupted_at_startup(&state.db)?;
    if jobs + chg > 0 {
        tracing::warn!("marked {jobs} job(s) and {chg} change(s) as interrupted");
    }
    routes::system::CpuSampler::spawn(state.clone());
    backups::spawn_scheduler(state.clone());
    deploys::spawn_poller(state.clone());
    monitor::spawn(state.clone());
    {
        let st = state.clone();
        tokio::spawn(async move { cron::startup(&st).await });
    }
    let app = routes::router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();

    let handle: axum_server::Handle<SocketAddr> = axum_server::Handle::new();
    tokio::spawn(shutdown_on_signal(handle.clone(), state.clone()));

    tracing::info!(
        "nexacloud {} listening on {}://{addr} (web_dir {}, backup_dir {}, data_dir {})",
        env!("CARGO_PKG_VERSION"),
        if tls.is_some() { "https" } else { "http" },
        state.config.web_dir.display(),
        state.config.backup_dir.display(),
        state.config.data_dir.display(),
    );
    match tls {
        Some(tls) => {
            axum_server::bind_rustls(addr, tls)
                .handle(handle)
                .serve(app)
                .await
        }
        None => axum_server::bind(addr).handle(handle).serve(app).await,
    }
    .with_context(|| format!("serving on {addr}"))?;
    tracing::info!("shut down");
    Ok(())
}

async fn shutdown_on_signal(handle: axum_server::Handle<SocketAddr>, state: AppState) {
    let ctrl_c = tokio::signal::ctrl_c();
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutting down");
    state.jobs.begin_shutdown();
    handle.graceful_shutdown(Some(Duration::from_secs(10)));
}
