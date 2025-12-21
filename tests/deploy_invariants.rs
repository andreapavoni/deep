use anyhow::Result;
use std::path::Path;
use std::process::{ExitStatus, Output};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

use deep::cli::deploy::{DeployArgs, handle_deploy};
use deep::config::{ConfigSnapshot, DeployConfig, HealthcheckConfig};
use deep::db::{ReleaseRow, Storage};
use deep::proxy::CaddyFile;
use deep::runner::{Runner, set_runner_for_tests};

#[derive(Default)]
struct TestRunner {
    rules: Mutex<Vec<Rule>>,
}

#[derive(Clone)]
struct Rule {
    contains: Vec<String>,
    status: i32,
    stdout: String,
    stderr: String,
}

impl Rule {
    fn matches(&self, cmd: &str) -> bool {
        self.contains.iter().all(|needle| cmd.contains(needle))
    }
}

impl TestRunner {
    fn add_rule(&self, contains: &[&str], status: i32, stdout: &str, stderr: &str) {
        self.rules.lock().expect("rules lock").push(Rule {
            contains: contains.iter().map(|s| s.to_string()).collect(),
            status,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        });
    }
}

impl Runner for TestRunner {
    fn output(&self, program: &str, args: &[&str]) -> Result<Output> {
        let args_joined = args.iter().copied().collect::<Vec<&str>>().join(" ");
        let cmdline = format!("{} {}", program, args_joined);
        if let Some(rule) = self
            .rules
            .lock()
            .expect("rules lock")
            .iter()
            .find(|rule| rule.matches(&cmdline))
            .cloned()
        {
            return Ok(Output {
                status: exit_status(rule.status),
                stdout: rule.stdout.into_bytes(),
                stderr: rule.stderr.into_bytes(),
            });
        }
        Ok(Output {
            status: exit_status(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
}

#[cfg(unix)]
fn exit_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(code << 8)
}

#[cfg(windows)]
fn exit_status(code: i32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(code as u32)
}

fn write_app_toml(path: &Path, quadlet_dir: &Path, retain: u32) -> Result<()> {
    let contents = format!(
        r#"[app]
name = "app"
port = 18080
domains = ["app.example.com"]

[deploy]
image = "ghcr.io/me/app:latest"
quadlet_dir = "{quadlet}"
retain = {retain}
"#,
        quadlet = quadlet_dir.display(),
        retain = retain
    );
    std::fs::write(path, contents)?;
    Ok(())
}

fn insert_release(
    storage: &mut Storage,
    app_id: &str,
    release_id: &str,
    created_at: &str,
    snapshot: &ConfigSnapshot,
    status: &str,
) -> Result<()> {
    let release = ReleaseRow {
        id: release_id.to_string(),
        app_id: app_id.to_string(),
        created_at: created_at.to_string(),
        git_sha: "deadbeef".to_string(),
        image_ref: "ghcr.io/me/app:latest".to_string(),
        image_digest: "ghcr.io/me/app@sha256:deadbeef".to_string(),
        config_json: serde_json::to_string(snapshot)?,
        status: status.to_string(),
    };
    let tx = storage.transaction()?;
    Storage::insert_release(&tx, &release)?;
    tx.commit()?;
    Ok(())
}

fn set_current(storage: &mut Storage, app_id: &str, release_id: &str) -> Result<()> {
    let tx = storage.transaction()?;
    Storage::set_current_release(&tx, app_id, release_id)?;
    tx.commit()?;
    Ok(())
}

fn base_snapshot(quadlet_dir: &Path, retain: u32) -> ConfigSnapshot {
    ConfigSnapshot {
        env: Default::default(),
        port: 18080,
        domains: vec!["app.example.com".to_string()],
        addons: Vec::new(),
        healthcheck: HealthcheckConfig::default(),
        deploy: DeployConfig {
            image: Some("ghcr.io/me/app:latest".to_string()),
            image_prefix: None,
            tag_strategy: None,
            git_ref: None,
            quadlet_dir: Some(quadlet_dir.to_string_lossy().to_string()),
            image_template: None,
            retain,
        },
    }
}

#[test]
fn deploy_start_failure_does_not_flip_current() -> Result<()> {
    let dir = TempDir::new()?;
    let db_path = dir.path().join("deep.db");
    let quadlet_dir = dir.path().join("quadlets");
    let app_toml = dir.path().join("app.toml");
    write_app_toml(&app_toml, &quadlet_dir, 5)?;

    let runner = Arc::new(TestRunner::default());
    runner.add_rule(&["command -v podman"], 0, "", "");
    runner.add_rule(
        &["systemctl", "--user", "enable", "--now", "deep-app-app-"],
        1,
        "",
        "unit failed",
    );
    let _guard = set_runner_for_tests(runner);

    let mut storage = Storage::open(&db_path)?;
    let app = storage.create_app("app", dir.path().to_string_lossy().as_ref())?;

    let snapshot = base_snapshot(&quadlet_dir, 5);
    insert_release(
        &mut storage,
        &app.id,
        "r1",
        "2024-01-01T00:00:00Z",
        &snapshot,
        "active",
    )?;
    set_current(&mut storage, &app.id, "r1")?;

    let proxy = CaddyFile::new(dir.path().join("Caddyfile"), "deep-caddy".to_string());
    let args = DeployArgs {
        app: "app".to_string(),
        image: None,
        git_sha: None,
        image_digest: None,
        health_path: None,
        health_tcp: false,
        health_retries: None,
        health_timeout_ms: None,
        health_interval_ms: None,
        skip_proxy: true,
        skip_pull: true,
        config: Some(app_toml),
        record_only: false,
        dry_run: false,
    };

    let result = handle_deploy(&mut storage, &proxy, args);
    assert!(result.is_err());

    let current = storage.current_release_id(&app.id)?;
    assert_eq!(current.as_deref(), Some("r1"));

    let releases = storage.list_releases(&app.id)?;
    let failed = releases.iter().find(|release| release.status == "failed");
    assert!(failed.is_some());
    Ok(())
}

#[test]
fn retention_prunes_old_releases() -> Result<()> {
    let dir = TempDir::new()?;
    let db_path = dir.path().join("deep.db");
    let quadlet_dir = dir.path().join("quadlets");
    let app_toml = dir.path().join("app.toml");
    write_app_toml(&app_toml, &quadlet_dir, 2)?;

    let runner = Arc::new(TestRunner::default());
    let _guard = set_runner_for_tests(runner);

    let mut storage = Storage::open(&db_path)?;
    let app = storage.create_app("app", dir.path().to_string_lossy().as_ref())?;

    let snapshot = base_snapshot(&quadlet_dir, 2);
    insert_release(
        &mut storage,
        &app.id,
        "r1",
        "2024-01-01T00:00:00Z",
        &snapshot,
        "active",
    )?;
    insert_release(
        &mut storage,
        &app.id,
        "r2",
        "2024-01-02T00:00:00Z",
        &snapshot,
        "active",
    )?;
    set_current(&mut storage, &app.id, "r2")?;

    let proxy = CaddyFile::new(dir.path().join("Caddyfile"), "deep-caddy".to_string());
    let args = DeployArgs {
        app: "app".to_string(),
        image: None,
        git_sha: None,
        image_digest: None,
        health_path: None,
        health_tcp: false,
        health_retries: None,
        health_timeout_ms: None,
        health_interval_ms: None,
        skip_proxy: true,
        skip_pull: true,
        config: Some(app_toml),
        record_only: true,
        dry_run: false,
    };

    handle_deploy(&mut storage, &proxy, args)?;

    let releases = storage.list_releases(&app.id)?;
    assert_eq!(releases.len(), 2);
    let ids: Vec<String> = releases.iter().map(|release| release.id.clone()).collect();
    assert!(ids.contains(&"r2".to_string()));
    assert!(!ids.contains(&"r1".to_string()));
    Ok(())
}
