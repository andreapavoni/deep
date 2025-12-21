use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::PathBuf;

use crate::cli::require_app;
use crate::db::Storage;
use crate::runtime::app_container_name;
use crate::systemd::{default_quadlet_dir, systemctl_for_dir};

#[derive(Subcommand, Debug)]
/// App-related commands.
pub enum AppsCommand {
    /// List apps
    #[command(alias = "ls")]
    List,
    /// Add an app and generate app.toml
    #[command(alias = "a")]
    Add {
        #[arg(help = "App name")]
        name: String,
        #[arg(
            short = 'r',
            long,
            help = "Path to bare git repo (for git push deploy)"
        )]
        repo_path: Option<String>,
        #[arg(
            short = 'c',
            long,
            default_value = "/srv/deep/apps",
            help = "Directory for generated app.toml"
        )]
        config_dir: PathBuf,
        #[arg(short = 'g', long, help = "Initialize bare repo and hook")]
        git: bool,
        #[arg(short = 't', long, help = "Image template for git hook")]
        image_template: Option<String>,
        #[arg(
            short = 'f',
            long,
            default_value = "Dockerfile",
            help = "Dockerfile path"
        )]
        dockerfile: String,
        #[arg(short = 'D', long, help = "Print actions without executing")]
        dry_run: bool,
    },
    /// Remove an app record
    #[command(alias = "rm")]
    Remove {
        #[arg(help = "App name")]
        name: String,
    },
    /// Start the current release
    #[command(alias = "st")]
    Start {
        #[arg(help = "App name")]
        name: String,
    },
    /// Stop the current release
    #[command(alias = "sp")]
    Stop {
        #[arg(help = "App name")]
        name: String,
    },
    /// Restart the current release
    #[command(alias = "rs")]
    Restart {
        #[arg(help = "App name")]
        name: String,
    },
}

/// Handle app subcommands.
pub fn handle(storage: &mut Storage, command: AppsCommand) -> Result<()> {
    match command {
        AppsCommand::List => {
            let apps = storage.list_apps()?;
            if apps.is_empty() {
                println!("no apps found");
                return Ok(());
            }
            for app in apps {
                println!("{}  {}", app.name, app.id);
            }
            Ok(())
        }
        AppsCommand::Add {
            name,
            repo_path,
            config_dir,
            git,
            image_template,
            dockerfile,
            dry_run,
        } => {
            let repo_path = repo_path.unwrap_or_else(|| format!("/srv/deep/repos/{}.git", name));
            let app_dir = config_dir.join(&name);
            let app_toml = app_dir.join("app.toml");
            if dry_run {
                print_add_plan(
                    &name,
                    &repo_path,
                    &config_dir,
                    git,
                    image_template.as_deref(),
                    &dockerfile,
                );
                return Ok(());
            }
            let app = storage.create_app(&name, &repo_path)?;
            if !app_toml.exists() {
                std::fs::create_dir_all(&app_dir)?;
                std::fs::write(&app_toml, default_app_toml(&name))?;
            }
            if git {
                let repo_path = crate::cli::git::init_repo_for_app(
                    storage,
                    &name,
                    PathBuf::from("/srv/deep/repos"),
                    Some(PathBuf::from(&repo_path)),
                    image_template,
                    &dockerfile,
                    "deep",
                )?;
                println!("initialized git repo {}", repo_path.display());
            }
            println!("created app {} ({})", app.name, app.id);
            println!("app config: {}", app_toml.display());
            Ok(())
        }
        AppsCommand::Remove { name } => {
            storage.remove_app(&name)?;
            println!("removed app {}", name);
            Ok(())
        }
        AppsCommand::Start { name } => app_action(storage, &name, "start"),
        AppsCommand::Stop { name } => app_action(storage, &name, "stop"),
        AppsCommand::Restart { name } => app_action(storage, &name, "restart"),
    }
}

fn default_app_toml(name: &str) -> String {
    let template = include_str!("../../templates/app.toml");
    template.replace("{{app}}", name)
}

fn app_action(storage: &mut Storage, name: &str, action: &str) -> Result<()> {
    let app_row = require_app(storage, name)?;
    let release_id = storage
        .current_release_id(&app_row.id)?
        .context("no current release set")?;
    let release = storage
        .get_release_by_id(&release_id)?
        .context("current release not found")?;
    let snapshot: crate::config::ConfigSnapshot =
        serde_json::from_str(&release.config_json).context("invalid release config")?;
    let quadlet_dir = snapshot
        .deploy
        .quadlet_dir
        .clone()
        .unwrap_or_else(default_quadlet_dir);
    let unit_name = app_container_name(&app_row.name, &release_id);
    let unit = format!("{}.service", unit_name);
    match action {
        "start" => systemctl_for_dir(&quadlet_dir, &["start", &unit])?,
        "stop" => systemctl_for_dir(&quadlet_dir, &["stop", &unit])?,
        "restart" => systemctl_for_dir(&quadlet_dir, &["restart", &unit])?,
        _ => anyhow::bail!("unknown app action {}", action),
    }
    println!("{} app {}", action, app_row.name);
    Ok(())
}

fn print_add_plan(
    name: &str,
    repo_path: &str,
    config_dir: &PathBuf,
    git: bool,
    image_template: Option<&str>,
    dockerfile: &str,
) {
    let app_dir = config_dir.join(name);
    let app_toml = app_dir.join("app.toml");
    println!("dry-run: apps add {}", name);
    println!("would create app record with repo_path={}", repo_path);
    println!("would write app config: {}", app_toml.display());
    if git {
        let hook_path = std::path::Path::new(repo_path)
            .join("hooks")
            .join("post-receive");
        println!("would init bare repo: {}", repo_path);
        println!("would write hook: {}", hook_path.display());
        println!("dockerfile={}", dockerfile);
        if let Some(template) = image_template {
            println!("image_template={}", template);
        } else {
            println!("image_template=from app.toml or default");
        }
    }
}
