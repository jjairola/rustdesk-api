//! Account and device management from the command line.
//!
//! Everything runs against the same SQLite file the server uses, so it can be
//! run alongside a live server (`docker compose exec`, or straight on the host).

use crate::{auth, db};
use anyhow::{bail, Result};
use clap::Subcommand;
use sqlx::SqlitePool;

#[derive(Subcommand)]
pub enum UserAction {
    /// Create a user account.
    Add {
        name: String,
        /// Mark the account as an administrator.
        #[arg(long)]
        admin: bool,
        /// Optional email, shown in the client's account panel.
        #[arg(long, default_value = "")]
        email: String,
        /// Read the password from this flag instead of prompting. Avoid on
        /// shared machines — it lands in your shell history.
        #[arg(long)]
        password: Option<String>,
    },
    /// List all accounts.
    List,
    /// Change an account's password.
    Passwd {
        name: String,
        #[arg(long)]
        password: Option<String>,
    },
    /// Delete an account and all of its sessions and address book entries.
    Rm { name: String },
}

#[derive(Subcommand)]
pub enum DeviceAction {
    /// List every registered workstation.
    List,
    /// Remove a workstation. It reappears if that machine reports in again.
    Rm { id: String },
}

pub async fn run_user(pool: &SqlitePool, action: UserAction) -> Result<()> {
    match action {
        UserAction::Add {
            name,
            admin,
            email,
            password,
        } => {
            if name.trim().is_empty() {
                bail!("username cannot be empty");
            }
            if db::find_user_by_name(pool, &name).await?.is_some() {
                bail!("user {name:?} already exists");
            }
            let password = resolve_password(password)?;
            let hash = auth::hash_password(&password)?;
            db::create_user(pool, &name, &hash, &email, admin).await?;
            println!(
                "created user {name:?}{}",
                if admin { " (admin)" } else { "" }
            );
        }

        UserAction::List => {
            let users = db::list_users(pool).await?;
            if users.is_empty() {
                println!("no users yet — create one with: rustdesk-api user add <name>");
                return Ok(());
            }
            println!("{:<24} {:<28} {:<7} STATUS", "NAME", "EMAIL", "ADMIN");
            for user in users {
                println!(
                    "{:<24} {:<28} {:<7} {}",
                    user.name,
                    if user.email.is_empty() { "-" } else { &user.email },
                    if user.is_admin != 0 { "yes" } else { "no" },
                    if user.status == 0 { "disabled" } else { "active" },
                );
            }
        }

        UserAction::Passwd { name, password } => {
            if db::find_user_by_name(pool, &name).await?.is_none() {
                bail!("no such user: {name}");
            }
            let password = resolve_password(password)?;
            let hash = auth::hash_password(&password)?;
            db::set_password(pool, &name, &hash).await?;
            println!("password updated for {name:?}");
        }

        UserAction::Rm { name } => {
            if db::delete_user(pool, &name).await? == 0 {
                bail!("no such user: {name}");
            }
            println!("deleted user {name:?}");
        }
    }
    Ok(())
}

pub async fn run_device(pool: &SqlitePool, action: DeviceAction) -> Result<()> {
    match action {
        DeviceAction::List => {
            // stale_days = 0 so the CLI always shows everything, including
            // machines the address book currently filters out.
            let devices = db::list_devices(pool, 0).await?;
            if devices.is_empty() {
                println!("no workstations have registered yet");
                return Ok(());
            }
            println!(
                "{:<12} {:<24} {:<10} {:<10} LAST SEEN",
                "ID", "HOSTNAME", "PLATFORM", "VERSION"
            );
            for device in devices {
                println!(
                    "{:<12} {:<24} {:<10} {:<10} {}",
                    device.id,
                    truncate(&device.hostname, 24),
                    device.platform,
                    device.version,
                    ago(device.last_seen),
                );
            }
        }

        DeviceAction::Rm { id } => {
            if db::delete_device(pool, &id).await? == 0 {
                bail!("no such device: {id}");
            }
            println!("deleted device {id:?}");
        }
    }
    Ok(())
}

/// Prompts twice and checks the two entries match, unless `--password` was given.
fn resolve_password(provided: Option<String>) -> Result<String> {
    if let Some(password) = provided {
        if password.is_empty() {
            bail!("password cannot be empty");
        }
        return Ok(password);
    }

    let password = rpassword::prompt_password("Password: ")?;
    if password.is_empty() {
        bail!("password cannot be empty");
    }
    let confirm = rpassword::prompt_password("Confirm password: ")?;
    if password != confirm {
        bail!("passwords do not match");
    }
    Ok(password)
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let kept: String = value.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn ago(timestamp: i64) -> String {
    let seconds = (db::now() - timestamp).max(0);
    match seconds {
        s if s < 90 => "just now".to_string(),
        s if s < 3_600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3_600),
        s => format!("{}d ago", s / 86_400),
    }
}
