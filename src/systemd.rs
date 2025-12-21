//! Helpers for running systemctl in user or system scope.

use crate::runner;
use anyhow::{Context, Result, bail};

/// Default quadlet directory based on $HOME.
pub fn default_quadlet_dir() -> String {
    if let Ok(home) = std::env::var("HOME") {
        return format!("{}/.config/containers/systemd", home);
    }
    "/etc/containers/systemd".to_string()
}

/// Check if a quadlet dir is a system-level path.
pub fn is_system_dir(dir: &str) -> bool {
    dir.starts_with("/etc/containers/systemd")
}

/// Run systemctl in the correct scope for a quadlet directory.
pub fn systemctl_for_dir(dir: &str, args: &[&str]) -> Result<()> {
    let mut cmd_args = Vec::new();
    if !is_system_dir(dir) {
        cmd_args.push("--user");
    }
    cmd_args.extend_from_slice(args);
    let status = runner::run_status("systemctl", &cmd_args)
        .with_context(|| format!("failed to run systemctl {:?}", args))?;
    if status.success() {
        Ok(())
    } else {
        bail!("systemctl failed: {:?}", args)
    }
}

/// Run systemctl --user first, then system scope as fallback.
pub fn systemctl_any(args: &[&str]) -> Result<()> {
    let mut user_args = vec!["--user"];
    user_args.extend_from_slice(args);
    if let Ok(status) = runner::run_status("systemctl", &user_args) {
        if status.success() {
            return Ok(());
        }
    }
    let status = runner::run_status("systemctl", args)
        .with_context(|| format!("failed to run systemctl {:?}", args))?;
    if status.success() {
        Ok(())
    } else {
        bail!("systemctl failed: {:?}", args)
    }
}

/// Check if a service is active in user or system scope.
pub fn systemctl_active_any(name: &str) -> Result<bool> {
    let status_user = runner::run_status(
        "systemctl",
        &["--user", "is-active", &format!("{}.service", name)],
    );
    if let Ok(status) = status_user {
        if status.success() {
            return Ok(true);
        }
    }
    let status = runner::run_status("systemctl", &["is-active", &format!("{}.service", name)])
        .with_context(|| "failed to run systemctl is-active")?;
    Ok(status.success())
}
