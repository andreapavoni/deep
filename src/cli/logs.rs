use anyhow::{Context, Result};
use clap::Args;

use crate::cli::require_app;
use crate::db::Storage;
use crate::runtime::{Runtime, app_container_name};

#[derive(Args, Debug)]
#[command(about = "Tail logs for the current release")]
/// Logs argument set.
pub struct LogsArgs {
    #[arg(help = "App name")]
    pub app: String,
    #[arg(short = 'f', long, help = "Follow log output")]
    pub follow: bool,
}

/// Handle log streaming for the current release.
pub fn handle(storage: &mut Storage, args: LogsArgs) -> Result<()> {
    let app_row = require_app(storage, &args.app)?;
    let release_id = storage
        .current_release_id(&app_row.id)?
        .context("no current release set")?;
    let runtime = Runtime::detect()?;
    let container_name = app_container_name(&app_row.name, &release_id);
    runtime.logs(&container_name, args.follow)?;
    Ok(())
}
