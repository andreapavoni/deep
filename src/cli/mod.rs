//! CLI entrypoints and command routing.

mod addons;
mod apps;
pub mod deploy;
pub mod git;
mod host;
mod image;
mod logs;
mod proxy;
mod releases;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

use crate::db::{AppRow, Storage};
use crate::proxy::CaddyFile;

#[derive(Parser, Debug)]
#[command(name = "deep", version, about = "Deep micro-PaaS CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Debug, Clone)]
/// Database connection options.
struct DbArgs {
    #[arg(
        short = 'd',
        long,
        default_value = "deep.db",
        help = "SQLite database path"
    )]
    db: PathBuf,
}

#[derive(Args, Debug, Clone)]
/// Proxy configuration overrides.
struct ProxyArgs {
    #[arg(
        short = 'f',
        long,
        default_value = "/srv/deep/caddy/config/Caddyfile",
        help = "Path to the host Caddyfile"
    )]
    caddyfile: PathBuf,
    #[arg(
        short = 'n',
        long,
        default_value = "deep-caddy",
        help = "Caddy service name"
    )]
    caddy_container: String,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Manage apps
    #[command(alias = "a")]
    Apps {
        #[command(flatten)]
        db: DbArgs,
        #[command(subcommand)]
        command: apps::AppsCommand,
    },
    /// Deploy a new release
    #[command(alias = "d")]
    Deploy {
        #[command(flatten)]
        db: DbArgs,
        #[command(flatten)]
        proxy: ProxyArgs,
        #[command(flatten)]
        args: deploy::DeployArgs,
    },
    /// Inspect releases
    #[command(alias = "r")]
    Releases {
        #[command(flatten)]
        db: DbArgs,
        #[command(subcommand)]
        command: releases::ReleasesCommand,
    },
    /// Roll back to a previous release
    #[command(alias = "rb")]
    Rollback {
        #[command(flatten)]
        db: DbArgs,
        #[command(flatten)]
        proxy: ProxyArgs,
        #[command(flatten)]
        args: deploy::RollbackArgs,
    },
    /// Stream logs for the current release
    #[command(alias = "l")]
    Logs {
        #[command(flatten)]
        db: DbArgs,
        #[command(flatten)]
        args: logs::LogsArgs,
    },
    /// Manage addons and bindings
    #[command(alias = "ad")]
    Addons {
        #[command(flatten)]
        db: DbArgs,
        #[command(subcommand)]
        command: addons::AddonsCommand,
    },
    /// Inspect and validate proxy routes
    #[command(alias = "p")]
    Proxy {
        #[command(flatten)]
        proxy: ProxyArgs,
        #[command(subcommand)]
        command: proxy::ProxyCommand,
    },
    /// Host setup and health checks
    #[command(alias = "h")]
    Host {
        #[command(flatten)]
        db: DbArgs,
        #[command(flatten)]
        proxy: ProxyArgs,
        #[command(subcommand)]
        command: host::HostCommand,
    },
    /// Manage git hook integration
    #[command(alias = "g")]
    Git {
        #[command(flatten)]
        db: DbArgs,
        #[command(subcommand)]
        command: git::GitCommand,
    },
    /// Build and publish images (laptop workflow)
    #[command(alias = "i")]
    Image {
        #[command(subcommand)]
        command: image::ImageCommand,
    },
}

/// Entry point for the CLI.
pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Apps { db, command } => {
            let mut storage = Storage::open(&db.db)?;
            apps::handle(&mut storage, command)
        }
        Command::Deploy { db, proxy, args } => {
            let mut storage = Storage::open(&db.db)?;
            let proxy = CaddyFile::new(proxy.caddyfile, proxy.caddy_container);
            deploy::handle_deploy(&mut storage, &proxy, args)
        }
        Command::Releases { db, command } => {
            let mut storage = Storage::open(&db.db)?;
            releases::handle(&mut storage, command)
        }
        Command::Rollback { db, proxy, args } => {
            let mut storage = Storage::open(&db.db)?;
            let proxy = CaddyFile::new(proxy.caddyfile, proxy.caddy_container);
            deploy::handle_rollback(&mut storage, &proxy, args)
        }
        Command::Logs { db, args } => {
            let mut storage = Storage::open(&db.db)?;
            logs::handle(&mut storage, args)
        }
        Command::Addons { db, command } => {
            let mut storage = Storage::open(&db.db)?;
            addons::handle(&mut storage, command)
        }
        Command::Proxy { proxy, command } => {
            let proxy = CaddyFile::new(proxy.caddyfile, proxy.caddy_container);
            proxy::handle(&proxy, command)
        }
        Command::Host { db, proxy, command } => {
            let mut storage = Storage::open(&db.db)?;
            let proxy = CaddyFile::new(proxy.caddyfile, proxy.caddy_container);
            host::handle(&mut storage, &proxy, command)
        }
        Command::Git { db, command } => {
            let mut storage = Storage::open(&db.db)?;
            git::handle(&mut storage, command)
        }
        Command::Image { command } => image::handle(command),
    }
}

fn require_app(storage: &mut Storage, name: &str) -> Result<AppRow> {
    storage
        .get_app_by_name(name)?
        .with_context(|| format!("app {} not found", name))
}

fn now_rfc3339() -> String {
    let fmt = time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&fmt)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn record_proxy_error(
    storage: &mut Storage,
    app_name: &str,
    release_id: &str,
    action: &str,
    err: &anyhow::Error,
) {
    let payload = serde_json::json!({
        "app": app_name,
        "release_id": release_id,
        "action": action,
        "error": err.to_string()
    });
    let _ = storage.insert_event("proxy_error", &payload.to_string());
}

fn resolve_config_path(
    args_config: &Option<PathBuf>,
    repo_path: &str,
    app_name: &str,
) -> Result<PathBuf> {
    if let Some(path) = args_config {
        return Ok(path.clone());
    }
    let app_dir = std::path::Path::new("/srv/deep/apps")
        .join(app_name)
        .join("app.toml");
    if app_dir.exists() {
        return Ok(app_dir);
    }
    let candidate = std::path::Path::new(repo_path).join("app.toml");
    if candidate.exists() {
        return Ok(candidate);
    }
    let local = std::path::Path::new("app.toml");
    if local.exists() {
        return Ok(local.to_path_buf());
    }
    bail!(
        "app.toml not found; pass --config or place app.toml at /srv/deep/apps/{}/app.toml",
        app_name
    )
}

fn resolve_healthcheck(
    snapshot: &crate::config::ConfigSnapshot,
    args: &deploy::DeployArgs,
) -> crate::config::HealthcheckConfig {
    let mut config = snapshot.healthcheck.clone();
    if let Some(path) = &args.health_path {
        config.path = path.clone();
    }
    if args.health_tcp {
        config.kind = crate::config::HealthcheckKind::Tcp;
    }
    if let Some(retries) = args.health_retries {
        config.retries = retries;
    }
    if let Some(timeout_ms) = args.health_timeout_ms {
        config.timeout_ms = timeout_ms;
    }
    if let Some(interval_ms) = args.health_interval_ms {
        config.interval_ms = interval_ms;
    }
    config
}
