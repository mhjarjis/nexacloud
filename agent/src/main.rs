//! NexaCloud agent: manages a VPS hosting Laravel and plain-PHP sites.

mod alerts;
mod auth;
mod config;
mod error;
mod jobs;
mod nginx;
mod routes;
mod state;
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Serve { config } => serve(config),
        Command::Init { config } => init(config),
        Command::ResetPassword { config } => reset_password(config),
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
                .with_context(|| format!("loading TLS cert {} / key {}", cert.display(), key.display()))?,
        )
    } else {
        None
    };

    let state = AppState::new(cfg);
    routes::system::CpuSampler::spawn(state.clone());
    let app = routes::router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();

    let handle: axum_server::Handle<SocketAddr> = axum_server::Handle::new();
    tokio::spawn(shutdown_on_signal(handle.clone()));

    tracing::info!(
        "nexacloud {} listening on {}://{addr} (web_dir {}, backup_dir {})",
        env!("CARGO_PKG_VERSION"),
        if tls.is_some() { "https" } else { "http" },
        state.config.web_dir.display(),
        state.config.backup_dir.display(),
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

async fn shutdown_on_signal(handle: axum_server::Handle<SocketAddr>) {
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
    handle.graceful_shutdown(Some(Duration::from_secs(10)));
}
