use anyhow::Result;
use std::path::PathBuf;
use std::process::{ExitStatus, Output};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

use deep::config::{ConfigSnapshot, DeployConfig, HealthcheckConfig};
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

fn snapshot() -> ConfigSnapshot {
    ConfigSnapshot {
        env: Default::default(),
        port: 3000,
        domains: vec!["app.example.com".to_string()],
        addons: Vec::new(),
        healthcheck: HealthcheckConfig::default(),
        deploy: DeployConfig::default(),
    }
}

#[test]
fn proxy_reload_failure_restores_previous_caddyfile() -> Result<()> {
    let dir = TempDir::new()?;
    let caddyfile = dir.path().join("Caddyfile");
    let initial = "# deep:app:old\nold.example.com {\n    reverse_proxy old:3000\n}\n# deep:end\n";
    std::fs::write(&caddyfile, initial)?;

    let runner = Arc::new(TestRunner::default());
    runner.add_rule(
        &["systemctl --user reload deep-caddy.service"],
        1,
        "",
        "reload failed",
    );
    runner.add_rule(
        &["systemctl reload deep-caddy.service"],
        1,
        "",
        "reload failed",
    );
    let _guard = set_runner_for_tests(runner);

    let proxy = CaddyFile::new(PathBuf::from(&caddyfile), "deep-caddy".to_string());
    let result = proxy.upsert_route("app", "r2", &snapshot());
    assert!(result.is_err());

    let current = std::fs::read_to_string(&caddyfile)?;
    assert_eq!(current, initial);

    let backup = caddyfile.with_extension("bak");
    let backup_contents = std::fs::read_to_string(&backup)?;
    assert_eq!(backup_contents, initial);
    Ok(())
}
