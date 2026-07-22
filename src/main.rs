mod api;
mod auth;
mod cli;
mod config;
mod db;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::Config;

#[derive(Parser)]
#[command(
    name = "rustdesk-api",
    version,
    about = "Minimal RustDesk API server — every registered workstation appears in every user's address book"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server (default).
    Serve,
    /// Manage user accounts.
    User {
        #[command(subcommand)]
        action: cli::UserAction,
    },
    /// Inspect and prune registered workstations.
    Device {
        #[command(subcommand)]
        action: cli::DeviceAction,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rustdesk_api=info,tower_http=warn".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = Config::from_env();
    let pool = db::connect(&config.database_url).await?;

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(config, pool).await,
        Command::User { action } => cli::run_user(&pool, action).await,
        Command::Device { action } => cli::run_device(&pool, action).await,
    }
}

async fn serve(config: Config, pool: sqlx::SqlitePool) -> Result<()> {
    bootstrap_admin(&config, &pool).await?;

    match db::purge_expired_tokens(&pool).await {
        Ok(n) if n > 0 => tracing::info!("purged {n} expired token(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("failed to purge expired tokens: {e:#}"),
    }

    if db::count_users(&pool).await? == 0 {
        tracing::warn!(
            "no user accounts exist — nobody can log in yet. \
             Create one with: rustdesk-api user add <name>"
        );
    }

    let app = api::router(api::AppState {
        pool,
        config: config.clone(),
    });

    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .with_context(|| format!("failed to bind {}", config.bind))?;

    tracing::info!("listening on http://{}", config.bind);
    tracing::info!(
        "point the RustDesk client's \"API Server\" setting at this address"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    Ok(())
}

/// Creates the first account from `RDAPI_ADMIN_USER` / `RDAPI_ADMIN_PASSWORD`,
/// but only while no users exist — so restarting a running deployment with
/// those variables still set will not resurrect or reset an account.
async fn bootstrap_admin(config: &Config, pool: &sqlx::SqlitePool) -> Result<()> {
    let (Some(name), Some(password)) = (&config.admin_user, &config.admin_password) else {
        return Ok(());
    };

    if db::count_users(pool).await? > 0 {
        return Ok(());
    }

    let hash = auth::hash_password(password)?;
    db::create_user(pool, name, &hash, "", true).await?;
    tracing::info!("created initial admin account {name:?} from environment");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutting down");
}
