use anyhow::{Context, Result, bail};
use clap::Args;
use std::collections::HashSet;
use ulid::Ulid;

use crate::cli::{
    now_rfc3339, record_proxy_error, require_app, resolve_config_path, resolve_healthcheck,
};
use crate::config::load_app_config;
use crate::db::{ReleaseRow, Storage};
use crate::proxy::CaddyFile;
use crate::runtime::{Runtime, app_container_name};
use crate::systemd::{default_quadlet_dir, systemctl_for_dir};

#[derive(Clone, Args, Debug)]
#[command(about = "Deploy a new release for an app")]
/// Deploy argument set.
pub struct DeployArgs {
    #[arg(help = "App name")]
    pub app: String,
    #[arg(short = 'i', long, help = "Image reference to deploy")]
    pub image: Option<String>,
    #[arg(short = 'g', long, help = "Git SHA to record for the release")]
    pub git_sha: Option<String>,
    #[arg(short = 'd', long, help = "Image digest to record (skip resolve)")]
    pub image_digest: Option<String>,
    #[arg(short = 'p', long, help = "HTTP healthcheck path override")]
    pub health_path: Option<String>,
    #[arg(short = 'T', long, help = "Use TCP healthcheck instead of HTTP")]
    pub health_tcp: bool,
    #[arg(short = 'r', long, help = "Healthcheck retry count override")]
    pub health_retries: Option<u32>,
    #[arg(short = 't', long, help = "Healthcheck timeout override (ms)")]
    pub health_timeout_ms: Option<u64>,
    #[arg(short = 'I', long, help = "Healthcheck interval override (ms)")]
    pub health_interval_ms: Option<u64>,
    #[arg(short = 'S', long, help = "Skip proxy update")]
    pub skip_proxy: bool,
    #[arg(short = 'P', long, help = "Skip image pull/digest resolve")]
    pub skip_pull: bool,
    #[arg(short = 'c', long, help = "Path to app.toml")]
    pub config: Option<std::path::PathBuf>,
    #[arg(short = 'R', long, help = "Record release without starting containers")]
    pub record_only: bool,
    #[arg(short = 'D', long, help = "Print actions without executing")]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
#[command(about = "Rollback an app to a previous release")]
/// Rollback argument set.
pub struct RollbackArgs {
    #[arg(help = "App name")]
    pub app: String,
    #[arg(help = "Release id to roll back to")]
    pub release_id: String,
    #[arg(short = 'D', long, help = "Print actions without executing")]
    pub dry_run: bool,
}

/// Deploy a new release for an app.
pub fn handle_deploy(storage: &mut Storage, proxy: &CaddyFile, args: DeployArgs) -> Result<()> {
    let app = require_app(storage, &args.app)?;
    let config_path = resolve_config_path(&args.config, &app.repo_path, &app.name)?;
    let config = load_app_config(&config_path)?;
    let addon_snapshots = storage.addon_snapshots_for_app(&app.id)?;
    let mut snapshot = config.to_snapshot(addon_snapshots);
    apply_addon_env(&mut snapshot);
    if snapshot.deploy.quadlet_dir.is_none() {
        snapshot.deploy.quadlet_dir = Some(default_quadlet_dir());
    }
    let healthcheck = resolve_healthcheck(&snapshot, &args);
    snapshot.healthcheck = healthcheck.clone();
    let git_sha_base = resolve_git_sha_base(snapshot.deploy.git_ref.clone(), &app.repo_path)?;
    let image_ref = resolve_image_ref(args.image.clone(), &snapshot, &git_sha_base)?;
    let config_json = serde_json::to_string(&snapshot)?;

    let runtime = if args.record_only {
        None
    } else {
        Some(Runtime::detect()?)
    };

    let image_digest = if args.record_only || args.skip_pull {
        args.clone().image_digest.unwrap_or_else(|| {
            eprintln!("warning: image digest not provided; using image ref as digest");
            image_ref.clone()
        })
    } else {
        match args.clone().image_digest {
            Some(digest) => digest.clone(),
            None => runtime
                .as_ref()
                .context("runtime required for image pull")?
                .pull_image(&image_ref)?,
        }
    };

    let git_sha = resolve_git_sha(args.git_sha.clone(), Some(git_sha_base.clone()), &image_ref)?;
    if args.dry_run {
        print_deploy_plan(&app.name, &snapshot, &image_ref, &git_sha, &args.clone())?;
        return Ok(());
    }

    let release_id = Ulid::new().to_string();
    let release = ReleaseRow {
        id: release_id.clone(),
        app_id: app.id.clone(),
        created_at: now_rfc3339(),
        git_sha,
        image_ref: image_ref.clone(),
        image_digest,
        config_json,
        status: "pending".to_string(),
    };

    let deployment_id = Ulid::new().to_string();
    let from_release_id = storage.current_release_id(&app.id)?;

    let tx = storage.transaction()?;
    Storage::insert_release(&tx, &release)?;
    Storage::insert_deployment(
        &tx,
        &deployment_id,
        &app.id,
        from_release_id.as_deref(),
        Some(&release_id),
        "pending",
        None,
    )?;
    tx.commit()?;

    if args.record_only {
        let tx = storage.transaction()?;
        Storage::set_current_release(&tx, &app.id, &release_id)?;
        tx.commit()?;
        storage.set_release_status(&release_id, "active")?;
        storage.update_deployment_status(&deployment_id, "succeeded", None)?;
        if let Err(err) = enforce_retention(storage, &app, &snapshot) {
            eprintln!("warning: retention failed: {}", err);
        }
        println!("recorded release {} for {}", release_id, app.name);
        return Ok(());
    }

    let runtime = runtime.context("runtime required for deploy")?;
    let container_name = app_container_name(&app.name, &release_id);
    let start_result = start_app_quadlet(&runtime, &app.name, &release_id, &snapshot, &image_ref);
    if let Err(err) = start_result {
        storage.set_release_status(&release_id, "failed")?;
        storage.update_deployment_status(&deployment_id, "failed", Some(&err.to_string()))?;
        return Err(err);
    }

    let health_result =
        runtime.healthcheck_with_config(&container_name, snapshot.port, &healthcheck);

    if let Err(err) = health_result {
        let _ = stop_app_release(storage, &app.name, &release_id);
        storage.set_release_status(&release_id, "failed")?;
        storage.update_deployment_status(&deployment_id, "failed", Some(&err.to_string()))?;
        return Err(err);
    }

    if !args.skip_proxy {
        if let Err(err) = proxy.upsert_route(&app.name, &release_id, &snapshot) {
            let _ = stop_app_release(storage, &app.name, &release_id);
            storage.set_release_status(&release_id, "failed")?;
            storage.update_deployment_status(&deployment_id, "failed", Some(&err.to_string()))?;
            record_proxy_error(storage, &app.name, &release_id, "deploy", &err);
            return Err(err);
        }
    }

    let tx = storage.transaction()?;
    Storage::set_current_release(&tx, &app.id, &release_id)?;
    tx.commit()?;
    storage.set_release_status(&release_id, "active")?;
    storage.update_deployment_status(&deployment_id, "succeeded", None)?;

    if let Some(old_release_id) = from_release_id {
        let _ = stop_app_release(storage, &app.name, &old_release_id);
    }
    if let Err(err) = enforce_retention(storage, &app, &snapshot) {
        eprintln!("warning: retention failed: {}", err);
    }

    println!("deployed {} as {}", app.name, release_id);
    Ok(())
}

fn resolve_image_ref(
    input: Option<String>,
    snapshot: &crate::config::ConfigSnapshot,
    git_sha: &str,
) -> Result<String> {
    if let Some(value) = input {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }
    if let Some(image) = snapshot.deploy.image.clone() {
        return Ok(image);
    }
    if let Some(prefix) = snapshot.deploy.image_prefix.clone() {
        let strategy = snapshot
            .deploy
            .tag_strategy
            .clone()
            .unwrap_or_else(|| "git_sha".to_string());
        let tag = match strategy.as_str() {
            "git_sha" => git_sha.to_string(),
            "latest" => "latest".to_string(),
            _ => bail!("unknown tag_strategy {}", strategy),
        };
        return Ok(format!("{}:{}", prefix, tag));
    }
    bail!("image ref required (pass --image or set [deploy].image or [deploy].image_prefix)");
}

fn resolve_git_sha_base(git_ref: Option<String>, repo_path: &str) -> Result<String> {
    if let Ok(repo) = git2::Repository::open(repo_path) {
        if let Some(reference) = git_ref {
            if let Ok(obj) = repo.revparse_single(&reference) {
                if let Some(oid) = obj.as_commit().map(|c| c.id()) {
                    return Ok(oid.to_string());
                }
            }
        }
        if let Ok(head) = repo.head() {
            if let Some(oid) = head.target() {
                return Ok(oid.to_string());
            }
        }
    }
    Ok("unknown".to_string())
}

fn resolve_git_sha(input: Option<String>, base: Option<String>, image_ref: &str) -> Result<String> {
    if let Some(value) = input {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }
    if let Some(value) = base {
        if value != "unknown" {
            return Ok(value);
        }
    }
    if let Some(tag) = extract_image_tag(image_ref) {
        return Ok(tag);
    }
    Ok("unknown".to_string())
}

fn extract_image_tag(image_ref: &str) -> Option<String> {
    if let Some((_, digest)) = image_ref.split_once('@') {
        return Some(digest.to_string());
    }
    let last_slash = image_ref.rfind('/').unwrap_or(0);
    if let Some(idx) = image_ref[last_slash..].rfind(':') {
        let tag = &image_ref[last_slash + idx + 1..];
        if !tag.is_empty() {
            return Some(tag.to_string());
        }
    }
    None
}

fn start_app_quadlet(
    runtime: &Runtime,
    app_name: &str,
    release_id: &str,
    snapshot: &crate::config::ConfigSnapshot,
    image_ref: &str,
) -> Result<()> {
    runtime.ensure_deep_network()?;
    let quadlet_dir = snapshot
        .deploy
        .quadlet_dir
        .clone()
        .unwrap_or_else(default_quadlet_dir);
    let unit_name = format!("deep-app-{}-{}", app_name, release_id);
    write_app_quadlet(
        &quadlet_dir,
        &unit_name,
        image_ref,
        snapshot,
        app_name,
        release_id,
    )?;
    systemctl_for_dir(&quadlet_dir, &["daemon-reload"])?;
    systemctl_for_dir(
        &quadlet_dir,
        &["enable", "--now", &format!("{}.service", unit_name)],
    )?;
    Ok(())
}

pub(crate) fn write_app_quadlet(
    quadlet_dir: &str,
    unit_name: &str,
    image_ref: &str,
    snapshot: &crate::config::ConfigSnapshot,
    app_name: &str,
    release_id: &str,
) -> Result<()> {
    let mut env_lines = Vec::new();
    for (key, value) in &snapshot.env {
        env_lines.push(format!("Environment={}={}", key, value));
    }
    env_lines.push(format!("Environment=PORT={}", snapshot.port));
    let quadlet_path = std::path::Path::new(quadlet_dir).join(format!("{}.container", unit_name));
    std::fs::create_dir_all(quadlet_dir)?;
    let template = include_str!("../../templates/app.container");
    let contents = template
        .replace("{{app}}", app_name)
        .replace("{{release}}", release_id)
        .replace("{{image}}", image_ref)
        .replace("{{env}}", &env_lines.join("\n"))
        .replace("{{health}}", &health_lines_for_snapshot(snapshot));
    std::fs::write(&quadlet_path, contents)?;
    Ok(())
}

/// Roll back to a previous release for an app.
pub fn handle_rollback(storage: &mut Storage, proxy: &CaddyFile, args: RollbackArgs) -> Result<()> {
    let app_row = require_app(storage, &args.app)?;
    let release = storage
        .get_release_by_id(&args.release_id)?
        .context("release not found")?;
    if release.app_id != app_row.id {
        bail!(
            "release {} does not belong to app {}",
            args.release_id,
            args.app
        );
    }
    let snapshot: crate::config::ConfigSnapshot =
        serde_json::from_str(&release.config_json).context("invalid release config")?;
    let healthcheck = snapshot.healthcheck.clone();

    if args.dry_run {
        print_rollback_plan(&app_row.name, &args.release_id, &snapshot)?;
        return Ok(());
    }

    let deployment_id = Ulid::new().to_string();
    let from_release_id = storage.current_release_id(&app_row.id)?;
    let tx = storage.transaction()?;
    Storage::insert_deployment(
        &tx,
        &deployment_id,
        &app_row.id,
        from_release_id.as_deref(),
        Some(&args.release_id),
        "pending",
        None,
    )?;
    tx.commit()?;

    let runtime = Runtime::detect()?;
    let container_name = app_container_name(&app_row.name, &args.release_id);
    if let Err(err) = start_app_quadlet(
        &runtime,
        &app_row.name,
        &args.release_id,
        &snapshot,
        &release.image_ref,
    ) {
        storage.update_deployment_status(&deployment_id, "failed", Some(&err.to_string()))?;
        return Err(err);
    }

    if let Err(err) = runtime.healthcheck_with_config(&container_name, snapshot.port, &healthcheck)
    {
        let _ = stop_app_release(storage, &app_row.name, &args.release_id);
        storage.update_deployment_status(&deployment_id, "failed", Some(&err.to_string()))?;
        return Err(err);
    }

    if let Err(err) = proxy.upsert_route(&app_row.name, &args.release_id, &snapshot) {
        let _ = stop_app_release(storage, &app_row.name, &args.release_id);
        storage.update_deployment_status(&deployment_id, "failed", Some(&err.to_string()))?;
        record_proxy_error(storage, &app_row.name, &args.release_id, "rollback", &err);
        return Err(err);
    }

    let tx = storage.transaction()?;
    Storage::set_current_release(&tx, &app_row.id, &args.release_id)?;
    tx.commit()?;
    storage.set_release_status(&args.release_id, "active")?;
    storage.update_deployment_status(&deployment_id, "succeeded", None)?;

    if let Some(old_release_id) = from_release_id {
        if old_release_id != args.release_id {
            let _ = stop_app_release(storage, &app_row.name, &old_release_id);
        }
    }
    if let Err(err) = enforce_retention(storage, &app_row, &snapshot) {
        eprintln!("warning: retention failed: {}", err);
    }

    println!("rolled back {} to {}", args.app, args.release_id);
    Ok(())
}

fn stop_app_release(storage: &mut Storage, app_name: &str, release_id: &str) -> Result<()> {
    let release = storage.get_release_by_id(release_id)?;
    if let Some(release) = release {
        let snapshot: crate::config::ConfigSnapshot = serde_json::from_str(&release.config_json)
            .unwrap_or(crate::config::ConfigSnapshot {
                env: Default::default(),
                port: 0,
                domains: Vec::new(),
                addons: Vec::new(),
                healthcheck: crate::config::HealthcheckConfig::default(),
                deploy: crate::config::DeployConfig::default(),
            });
        let unit_name = app_container_name(app_name, release_id);
        let quadlet_dir = snapshot
            .deploy
            .quadlet_dir
            .clone()
            .unwrap_or_else(default_quadlet_dir);
        let _ = systemctl_for_dir(&quadlet_dir, &["stop", &format!("{}.service", unit_name)]);
    }
    Ok(())
}

pub(crate) fn apply_addon_env(snapshot: &mut crate::config::ConfigSnapshot) {
    for addon in &snapshot.addons {
        if let Some(env) = addon.config.get("env").and_then(|value| value.as_object()) {
            for (key, value) in env {
                if let Some(val) = value.as_str() {
                    snapshot.env.insert(key.clone(), val.to_string());
                }
            }
        }
    }
}

fn enforce_retention(
    storage: &mut Storage,
    app: &crate::db::AppRow,
    snapshot: &crate::config::ConfigSnapshot,
) -> Result<()> {
    let retain = snapshot.deploy.retain.max(1) as usize;
    let releases = storage.list_releases(&app.id)?;
    if releases.len() <= retain {
        return Ok(());
    }
    let current_id = storage.current_release_id(&app.id)?;
    let mut keep: HashSet<String> = HashSet::new();
    if let Some(current_id) = current_id {
        keep.insert(current_id);
    }
    for release in &releases {
        if keep.len() >= retain {
            break;
        }
        keep.insert(release.id.clone());
    }
    for release in releases {
        if keep.contains(&release.id) {
            continue;
        }
        prune_release(storage, app, &release)?;
    }
    Ok(())
}

fn prune_release(
    storage: &mut Storage,
    app: &crate::db::AppRow,
    release: &ReleaseRow,
) -> Result<()> {
    let snapshot: crate::config::ConfigSnapshot = serde_json::from_str(&release.config_json)
        .unwrap_or(crate::config::ConfigSnapshot {
            env: Default::default(),
            port: 0,
            domains: Vec::new(),
            addons: Vec::new(),
            healthcheck: crate::config::HealthcheckConfig::default(),
            deploy: crate::config::DeployConfig::default(),
        });
    let unit_name = app_container_name(&app.name, &release.id);
    let quadlet_dir = snapshot
        .deploy
        .quadlet_dir
        .clone()
        .unwrap_or_else(default_quadlet_dir);
    let unit = format!("{}.service", unit_name);
    let _ = systemctl_for_dir(&quadlet_dir, &["stop", &unit]);
    let _ = systemctl_for_dir(&quadlet_dir, &["disable", &unit]);
    let quadlet_path = std::path::Path::new(&quadlet_dir).join(format!("{}.container", unit_name));
    let _ = std::fs::remove_file(&quadlet_path);
    let _ = systemctl_for_dir(&quadlet_dir, &["daemon-reload"]);

    storage.delete_deployments_for_release(&release.id)?;
    storage.delete_release(&release.id)?;
    println!("pruned release {} for {}", release.id, app.name);
    Ok(())
}

fn print_deploy_plan(
    app_name: &str,
    snapshot: &crate::config::ConfigSnapshot,
    image_ref: &str,
    git_sha: &str,
    args: &DeployArgs,
) -> Result<()> {
    println!("dry-run: deploy {}", app_name);
    println!("image_ref={}", image_ref);
    println!("git_sha={}", git_sha);
    println!(
        "healthcheck={:?} retries={} timeout_ms={} interval_ms={}",
        snapshot.healthcheck.kind,
        snapshot.healthcheck.retries,
        snapshot.healthcheck.timeout_ms,
        snapshot.healthcheck.interval_ms
    );
    if args.record_only {
        println!("would record release without starting a container");
        return Ok(());
    }
    if args.skip_pull {
        println!("image_digest=not resolved (skip_pull)");
    } else if args.image_digest.is_some() {
        println!("image_digest=provided");
    } else {
        println!("image_digest=would resolve via podman pull");
    }
    println!("would create quadlet: deep-app-{}-<release_id>", app_name);
    println!("would healthcheck container on port {}", snapshot.port);
    if args.skip_proxy {
        println!("would skip proxy update");
    } else {
        println!("would update Caddy routes for {}", app_name);
    }
    println!("would set current release and stop previous release");
    Ok(())
}

fn print_rollback_plan(
    app_name: &str,
    release_id: &str,
    snapshot: &crate::config::ConfigSnapshot,
) -> Result<()> {
    println!("dry-run: rollback {}", app_name);
    println!("target_release={}", release_id);
    println!("would start quadlet: deep-app-{}-{}", app_name, release_id);
    println!("would healthcheck container on port {}", snapshot.port);
    println!("would update Caddy routes for {}", app_name);
    println!("would set current release and stop previous release");
    Ok(())
}

fn health_lines_for_snapshot(snapshot: &crate::config::ConfigSnapshot) -> String {
    let command = match snapshot.healthcheck.command.as_ref() {
        Some(cmd) if !cmd.trim().is_empty() => cmd.trim(),
        _ => return String::new(),
    };
    let interval = format_duration_ms(snapshot.healthcheck.interval_ms);
    let timeout = format_duration_ms(snapshot.healthcheck.timeout_ms);
    format!(
        "HealthCmd={}\nHealthInterval={}\nHealthTimeout={}\nHealthRetries={}",
        command, interval, timeout, snapshot.healthcheck.retries
    )
}

fn format_duration_ms(ms: u64) -> String {
    if ms % 1000 == 0 {
        format!("{}s", ms / 1000)
    } else {
        format!("{}ms", ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_app_quadlet_renders_env_and_health() -> Result<()> {
        let dir = TempDir::new()?;
        let quadlet_dir = dir.path().join("quadlets");
        let mut snapshot = crate::config::ConfigSnapshot {
            env: Default::default(),
            port: 4321,
            domains: vec!["app.example.com".to_string()],
            addons: Vec::new(),
            healthcheck: crate::config::HealthcheckConfig::default(),
            deploy: crate::config::DeployConfig::default(),
        };
        snapshot.env.insert("FOO".to_string(), "bar".to_string());
        snapshot.healthcheck.command = Some("curl -f http://localhost:4321/health".to_string());
        snapshot.healthcheck.interval_ms = 1500;
        snapshot.healthcheck.timeout_ms = 2500;
        snapshot.healthcheck.retries = 3;

        write_app_quadlet(
            quadlet_dir.to_string_lossy().as_ref(),
            "deep-app-app-r1",
            "ghcr.io/me/app:latest",
            &snapshot,
            "app",
            "r1",
        )?;

        let quadlet_path = quadlet_dir.join("deep-app-app-r1.container");
        let contents = std::fs::read_to_string(&quadlet_path)?;
        assert!(contents.contains("Image=ghcr.io/me/app:latest"));
        assert!(contents.contains("ContainerName=deep-app-app-r1"));
        assert!(contents.contains("Environment=FOO=bar"));
        assert!(contents.contains("Environment=PORT=4321"));
        assert!(contents.contains("HealthCmd=curl -f http://localhost:4321/health"));
        assert!(contents.contains("HealthInterval=1500ms"));
        assert!(contents.contains("HealthTimeout=2500ms"));
        assert!(contents.contains("HealthRetries=3"));
        Ok(())
    }
}
