use anyhow::Result;
use std::net::TcpListener;
use std::path::Path;
use std::process::{ExitStatus, Output};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

use deep::cli::deploy::{DeployArgs, RollbackArgs, handle_deploy, handle_rollback};
use deep::db::Storage;
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

fn write_app_toml(path: &Path, quadlet_dir: &Path, port: u16) -> Result<()> {
    let contents = format!(
        r#"[app]
name = "app"
port = {port}
domains = ["app.example.com"]

[healthcheck]
kind = "tcp"

[deploy]
image = "ghcr.io/me/app:latest"
quadlet_dir = "{quadlet}"
retain = 5
"#,
        quadlet = quadlet_dir.display(),
        port = port
    );
    std::fs::write(path, contents)?;
    Ok(())
}

#[test]
fn deploy_then_rollback_switches_current_and_routes() -> Result<()> {
    let dir = TempDir::new()?;
    let db_path = dir.path().join("deep.db");
    let quadlet_dir = dir.path().join("quadlets");
    let app_toml = dir.path().join("app.toml");
    let caddyfile = dir.path().join("Caddyfile");

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    write_app_toml(&app_toml, &quadlet_dir, port)?;

    let runner = Arc::new(TestRunner::default());
    runner.add_rule(&["command -v podman"], 0, "", "");
    runner.add_rule(
        &["podman image inspect --format {{index .RepoDigests 0}} ghcr.io/me/app:latest"],
        0,
        "ghcr.io/me/app@sha256:abcd",
        "",
    );
    runner.add_rule(
        &[
            "podman inspect --format {{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}} deep-app-app-",
        ],
        0,
        "127.0.0.1",
        "",
    );
    let _guard = set_runner_for_tests(runner);

    let mut storage = Storage::open(&db_path)?;
    let app_row = storage.create_app("app", dir.path().to_string_lossy().as_ref())?;

    let proxy = CaddyFile::new(caddyfile.clone(), "deep-caddy".to_string());
    let record_args = DeployArgs {
        app: "app".to_string(),
        image: None,
        git_sha: None,
        image_digest: None,
        health_path: None,
        health_tcp: false,
        health_retries: None,
        health_timeout_ms: None,
        health_interval_ms: None,
        skip_proxy: false,
        skip_pull: true,
        config: Some(app_toml.clone()),
        record_only: true,
        dry_run: false,
    };
    handle_deploy(&mut storage, &proxy, record_args)?;

    let first_release = storage.current_release_id(&app_row.id)?;
    let first_release = first_release.expect("first release");

    let deploy_args = DeployArgs {
        app: "app".to_string(),
        image: None,
        git_sha: None,
        image_digest: None,
        health_path: None,
        health_tcp: false,
        health_retries: None,
        health_timeout_ms: None,
        health_interval_ms: None,
        skip_proxy: false,
        skip_pull: false,
        config: Some(app_toml.clone()),
        record_only: false,
        dry_run: false,
    };
    handle_deploy(&mut storage, &proxy, deploy_args)?;

    let second_release = storage.current_release_id(&app_row.id)?;
    let second_release = second_release.expect("second release");
    assert_ne!(first_release, second_release);

    let caddy_contents = std::fs::read_to_string(&caddyfile)?;
    assert!(caddy_contents.contains(&format!("deep-app-app-{}", second_release)));

    let rollback_args = RollbackArgs {
        app: "app".to_string(),
        release_id: first_release.clone(),
        dry_run: false,
    };
    handle_rollback(&mut storage, &proxy, rollback_args)?;

    let current = storage.current_release_id(&app_row.id)?;
    assert_eq!(current.as_deref(), Some(first_release.as_str()));

    let caddy_after = std::fs::read_to_string(&caddyfile)?;
    assert!(caddy_after.contains(&format!("deep-app-app-{}", first_release)));

    drop(listener);
    Ok(())
}
