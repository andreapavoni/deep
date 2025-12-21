use anyhow::{Context, Result};
use clap::Subcommand;

use crate::cli::require_app;
use crate::db::Storage;

#[derive(Subcommand, Debug)]
/// Release-related commands.
pub enum ReleasesCommand {
    /// List releases for an app
    #[command(alias = "ls")]
    List {
        #[arg(help = "App name")]
        app: String,
    },
    /// Show the current release for an app
    #[command(alias = "cur")]
    Current {
        #[arg(help = "App name")]
        app: String,
    },
}

/// Handle release subcommands.
pub fn handle(storage: &mut Storage, command: ReleasesCommand) -> Result<()> {
    match command {
        ReleasesCommand::List { app } => {
            let app_row = require_app(storage, &app)?;
            let releases = storage.list_releases(&app_row.id)?;
            if releases.is_empty() {
                println!("no releases for {}", app);
                return Ok(());
            }
            for release in releases {
                println!(
                    "{}  {}  {}  {}",
                    release.id, release.status, release.git_sha, release.image_ref
                );
            }
            Ok(())
        }
        ReleasesCommand::Current { app } => {
            let app_row = require_app(storage, &app)?;
            let current = storage
                .current_release_id(&app_row.id)?
                .context("no current release set")?;
            let release = storage
                .get_release_by_id(&current)?
                .context("current release missing")?;
            println!(
                "{}  {}  {}  {}",
                release.id, release.status, release.git_sha, release.image_ref
            );
            Ok(())
        }
    }
}
