mod api;
mod auth;
mod compose_files;
mod config;
mod containers;
mod data;
mod db;
mod images;
mod jobs;
mod logs;
mod monitor;
mod ntfy;
mod onboarding;
mod recovery;
mod remote;
mod scheduler;
mod settings;
mod ssh;
mod stack_data;
mod terminal;
mod updates;

use clap::{Parser, Subcommand};
use std::{path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(version, about = "Compose administration over SSH")]
struct Cli {
    #[arg(long, default_value = "dockmin.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Commands>,
}
#[derive(Subcommand)]
enum Commands {
    SetPassword,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = config::Config::load(&cli.config)?;
    let database = db::Database::open(&cfg.database)?;
    if matches!(cli.command, Some(Commands::SetPassword)) {
        // Read from stdin so credentials do not appear in argv or process listings.
        use std::io::{self, BufRead};
        eprintln!("Enter a password on stdin (at least 12 characters):");
        let password = io::stdin()
            .lock()
            .lines()
            .next()
            .transpose()?
            .unwrap_or_default();
        anyhow::ensure!(
            password.len() >= 12 && password.len() <= 1024,
            "Password must be 12–1024 bytes"
        );
        database.set_password(&auth::hash_password(&password)?)?;
        eprintln!("Administrator password saved. Existing sessions revoked.");
        return Ok(());
    }
    anyhow::ensure!(
        database.password()?.is_some(),
        "Initialize the administrator password using set-password first"
    );
    anyhow::ensure!(
        cfg.static_dir.join("index.html").is_file(),
        "Build the frontend first: web/dist/index.html is missing"
    );
    database.recover()?;
    let state = Arc::new(api::AppState::new(cfg, database)?);
    monitor::start(state.clone());
    scheduler::start(state.clone());
    ntfy::start(state.clone());
    let listener = tokio::net::TcpListener::bind(&state.config.listen).await?;
    eprintln!("Dockmin listening on {}", state.config.listen);
    let shutdown_state = state.clone();
    axum::serve(listener, api::router(state))
        .with_graceful_shutdown(async move {
            shutdown().await;
            shutdown_state
                .stopping
                .store(true, std::sync::atomic::Ordering::SeqCst);
            shutdown_state.shutdown_changed.notify_waiters();
        })
        .await?;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
