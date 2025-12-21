use anyhow::{Context, Result, bail};
use clap::Subcommand;
use std::path::{Path, PathBuf};

use crate::config::load_app_config;
use crate::db::Storage;

#[derive(Subcommand, Debug)]
/// Git hook maintenance commands.
pub enum GitCommand {
    /// Update the post-receive hook for an app
    #[command(alias = "u")]
    UpdateHook {
        #[arg(help = "App name")]
        app: String,
        #[arg(
            short = 'r',
            long,
            default_value = "/srv/deep/repos",
            help = "Repos directory"
        )]
        repos_dir: PathBuf,
        #[arg(short = 'p', long, help = "Path to bare git repo")]
        repo_path: Option<PathBuf>,
        #[arg(short = 't', long, help = "Image template for git hook")]
        image_template: Option<String>,
        #[arg(
            short = 'f',
            long,
            default_value = "Dockerfile",
            help = "Dockerfile path"
        )]
        dockerfile: String,
        #[arg(
            short = 'b',
            long,
            default_value = "deep",
            help = "Path to deep binary"
        )]
        deep_bin: String,
    },
}

/// Handle git hook related commands.
pub fn handle(storage: &mut Storage, command: GitCommand) -> Result<()> {
    match command {
        GitCommand::UpdateHook {
            app,
            repos_dir,
            repo_path,
            image_template,
            dockerfile,
            deep_bin,
        } => handle_update_hook(
            storage,
            &app,
            repos_dir,
            repo_path,
            image_template,
            &dockerfile,
            &deep_bin,
        ),
    }
}

/// Initialize a bare repo and install the post-receive hook for an app.
pub fn init_repo_for_app(
    storage: &mut Storage,
    app: &str,
    repos_dir: PathBuf,
    repo_path: Option<PathBuf>,
    image_template: Option<String>,
    dockerfile: &str,
    deep_bin: &str,
) -> Result<PathBuf> {
    let app_row = storage
        .get_app_by_name(app)?
        .with_context(|| format!("app {} not found; create it first", app))?;
    let image_template = image_template.or_else(|| load_image_template(&app_row.repo_path, app));
    let repo_path = repo_path.unwrap_or_else(|| repos_dir.join(format!("{}.git", app)));
    if let Some(parent) = repo_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    init_bare_repo(&repo_path)?;
    write_post_receive(
        &repo_path,
        app,
        image_template.as_deref(),
        dockerfile,
        deep_bin,
    )?;

    Ok(repo_path)
}

fn init_bare_repo(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    git2::Repository::init_bare(path).context("failed to init bare repo")?;
    Ok(())
}

fn write_post_receive(
    repo_path: &Path,
    app: &str,
    image_template: Option<&str>,
    dockerfile: &str,
    deep_bin: &str,
) -> Result<()> {
    let hook_dir = repo_path.join("hooks");
    std::fs::create_dir_all(&hook_dir)?;
    let hook_path = hook_dir.join("post-receive");
    let image_template = image_template.unwrap_or("ghcr.io/me/{{app}}:{{sha}}");
    let build_block = format!(
        r#"
tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT
git --work-tree "$tmpdir" checkout -f "$newrev"
podman build -t "$image" -f "{dockerfile}" "$tmpdir"
"#,
        dockerfile = dockerfile
    );

    let script = format!(
        r#"#!/usr/bin/env sh
set -eu
read oldrev newrev refname
app="{app}"
image_template="{image_template}"
image=$(printf "%s" "$image_template" | sed "s/{{{{app}}}}/$app/g" | sed "s/{{{{sha}}}}/$newrev/g")
{build_block}
{deep_bin} deploy "$app" --git-sha "$newrev" --image "$image" --skip-pull
"#,
        app = app,
        image_template = image_template,
        deep_bin = deep_bin,
        build_block = build_block
    );
    std::fs::write(&hook_path, script)?;
    let mut perms = std::fs::metadata(&hook_path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook_path, perms)?;
    }
    Ok(())
}

fn handle_update_hook(
    storage: &mut Storage,
    app: &str,
    repos_dir: PathBuf,
    repo_path: Option<PathBuf>,
    image_template: Option<String>,
    dockerfile: &str,
    deep_bin: &str,
) -> Result<()> {
    let app_row = storage
        .get_app_by_name(app)?
        .with_context(|| format!("app {} not found; create it first", app))?;
    let repo_path = repo_path.unwrap_or_else(|| {
        if !app_row.repo_path.is_empty() {
            PathBuf::from(&app_row.repo_path)
        } else {
            repos_dir.join(format!("{}.git", app))
        }
    });
    if !repo_path.exists() {
        bail!("repo path {} does not exist", repo_path.display());
    }
    write_post_receive(
        &repo_path,
        app,
        image_template.as_deref(),
        dockerfile,
        deep_bin,
    )?;
    println!("updated hook for {}", repo_path.display());
    Ok(())
}

fn load_image_template(repo_path: &str, app: &str) -> Option<String> {
    let app_dir = std::path::Path::new("/srv/deep/apps")
        .join(app)
        .join("app.toml");
    if app_dir.exists() {
        return load_app_config(&app_dir)
            .ok()
            .and_then(|cfg| cfg.deploy.image_template);
    }
    let path = std::path::Path::new(repo_path).join("app.toml");
    if !path.exists() {
        return None;
    }
    load_app_config(&path)
        .ok()
        .and_then(|cfg| cfg.deploy.image_template)
}
