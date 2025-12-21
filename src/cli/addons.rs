use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::deploy::{apply_addon_env, write_app_quadlet};
use crate::cli::require_app;
use crate::db::{AddonRow, AppRow, Storage};
use crate::runner;
use crate::runtime::Runtime;
use crate::systemd::{default_quadlet_dir, systemctl_for_dir};

const DEFAULT_ADDON_DIR: &str = "/srv/deep/addons";

#[derive(Subcommand, Debug)]
/// Addon-related commands.
pub enum AddonsCommand {
    /// List addons
    #[command(alias = "ls")]
    List {
        #[arg(short = 'C', long, default_value = DEFAULT_ADDON_DIR, help = "Addon config directory")]
        config_dir: PathBuf,
    },
    /// Create an addon (quadlet-backed)
    #[command(alias = "a")]
    Create {
        #[arg(help = "Addon kind (e.g. postgres, redis)")]
        kind: String,
        #[arg(help = "Addon name")]
        name: String,
        #[arg(short = 'j', long, default_value = "{}", help = "Addon config JSON")]
        config_json: String,
        #[arg(short = 'c', long, help = "Addon config TOML file")]
        config: Option<PathBuf>,
        #[arg(short = 'C', long, default_value = DEFAULT_ADDON_DIR, help = "Addon config directory")]
        config_dir: PathBuf,
    },
    /// Destroy an addon record
    #[command(alias = "rm")]
    Destroy {
        #[arg(help = "Addon name")]
        name: String,
        #[arg(short = 'C', long, default_value = DEFAULT_ADDON_DIR, help = "Addon config directory")]
        config_dir: PathBuf,
    },
    /// Start an addon service
    #[command(alias = "st")]
    Start {
        #[arg(help = "Addon name")]
        name: String,
    },
    /// Stop an addon service
    #[command(alias = "sp")]
    Stop {
        #[arg(help = "Addon name")]
        name: String,
    },
    /// Restart an addon service
    #[command(alias = "rs")]
    Restart {
        #[arg(help = "Addon name")]
        name: String,
    },
    /// Bind an addon to an app
    #[command(alias = "b")]
    Bind {
        #[arg(help = "Addon name")]
        addon: String,
        #[arg(help = "App name")]
        app: String,
        #[arg(short = 'C', long, default_value = DEFAULT_ADDON_DIR, help = "Addon config directory")]
        config_dir: PathBuf,
    },
    /// Unbind an addon from an app
    #[command(alias = "ub")]
    Unbind {
        #[arg(help = "Addon name")]
        addon: String,
        #[arg(help = "App name")]
        app: String,
        #[arg(short = 'C', long, default_value = DEFAULT_ADDON_DIR, help = "Addon config directory")]
        config_dir: PathBuf,
    },
}

/// Handle addon subcommands.
pub fn handle(storage: &mut Storage, command: AddonsCommand) -> Result<()> {
    match command {
        AddonsCommand::List { config_dir } => {
            let addons = list_addon_configs(&config_dir)?;
            if addons.is_empty() {
                println!("no addons found");
                return Ok(());
            }
            for addon in addons {
                println!(
                    "{}  {}  {}",
                    addon.name,
                    addon.kind.as_deref().unwrap_or("unknown"),
                    addon.image
                );
            }
            Ok(())
        }
        AddonsCommand::Create {
            kind,
            name,
            config_json,
            config,
            config_dir,
        } => {
            let mut addon_config = if let Some(path) = config {
                load_addon_config_file(&path)?
            } else {
                addon_config_from_json(&config_json, &kind)?
            };
            if addon_config.kind.is_none() {
                addon_config.kind = Some(kind.clone());
            }
            if addon_config.kind.as_deref() != Some(kind.as_str()) {
                bail!(
                    "addon kind mismatch: config has {:?}, CLI has {}",
                    addon_config.kind,
                    kind
                );
            }
            ensure_addon_dir(&config_dir)?;
            let config_path = addon_config_path(&config_dir, &name);
            write_addon_config_file(&config_path, &addon_config)?;
            let config_json = addon_config_to_json(&addon_config)?;
            require_addon_image(&addon_config)?;
            let addon = storage.upsert_addon(&name, &kind, &config_json)?;
            maybe_start_addon_quadlet(&name, &addon_config)?;
            println!("created addon {} ({})", addon.name, addon.id);
            println!("addon config: {}", config_path.display());
            Ok(())
        }
        AddonsCommand::Destroy { name, config_dir } => {
            let config_path = addon_config_path(&config_dir, &name);
            if config_path.exists() {
                std::fs::remove_file(&config_path)?;
            }
            storage.destroy_addon(&name)?;
            println!("destroyed addon {}", name);
            Ok(())
        }
        AddonsCommand::Start { name } => addon_action(&name, "start"),
        AddonsCommand::Stop { name } => addon_action(&name, "stop"),
        AddonsCommand::Restart { name } => addon_action(&name, "restart"),
        AddonsCommand::Bind {
            addon,
            app,
            config_dir,
        } => {
            let app_row = require_app(storage, &app)?;
            let addon_config = load_addon_config_by_name(&config_dir, &addon)?;
            let kind = addon_config
                .kind
                .clone()
                .unwrap_or_else(|| "generic".to_string());
            let config_json = addon_config_to_json(&addon_config)?;
            let addon_row = storage.upsert_addon(&addon, &kind, &config_json)?;
            let binding_env = provision_addon_on_bind(&addon_row, &addon_config, &app_row)?;
            let binding_json = serde_json::json!({ "env": binding_env }).to_string();
            storage.bind_addon(&app_row.id, &addon_row.id, &binding_json)?;
            restart_app_with_bindings(storage, &app_row)?;
            println!("bound addon {} to {}", addon, app);
            Ok(())
        }
        AddonsCommand::Unbind {
            addon,
            app,
            config_dir: _,
        } => {
            let app_row = require_app(storage, &app)?;
            let addon_row = storage
                .get_addon_by_name(&addon)?
                .context("addon not found")?;
            storage.unbind_addon(&app_row.id, &addon_row.id)?;
            restart_app_with_bindings(storage, &app_row)?;
            println!("unbound addon {} from {}", addon, app);
            Ok(())
        }
    }
}

fn maybe_start_addon_quadlet(name: &str, config: &AddonConfigFile) -> Result<()> {
    let runtime = Runtime::detect()?;
    runtime.ensure_deep_network()?;
    let image = config.image.as_str();
    let env = &config.env;
    let volumes = &config.volumes;
    let ports = &config.ports;
    if !ports.is_empty() {
        eprintln!(
            "warning: addon {} publishes ports to the host; omit ports to keep it internal",
            name
        );
    }
    let network = config.network.as_deref().unwrap_or("deep-net");
    let unit_name = format!("deep-addon-{}", name);
    let quadlet_dir = default_quadlet_dir();
    let quadlet_path = std::path::Path::new(&quadlet_dir).join(format!("{}.container", unit_name));
    std::fs::create_dir_all(&quadlet_dir)?;
    let mut env_lines = Vec::new();
    for (key, value) in env {
        env_lines.push(format!("Environment={}={}", key, value));
    }
    let mut volume_lines = Vec::new();
    for volume in volumes.iter().cloned() {
        volume_lines.push(format!("Volume={}", volume));
    }
    let mut port_lines = Vec::new();
    for port in ports.iter().cloned() {
        port_lines.push(format!("PublishPort={}", port));
    }
    let template = include_str!("../../templates/addon.container");
    let contents = template
        .replace("{{name}}", name)
        .replace("{{image}}", image)
        .replace("{{network}}", network)
        .replace("{{env}}", &env_lines.join("\n"))
        .replace("{{volumes}}", &volume_lines.join("\n"))
        .replace("{{ports}}", &port_lines.join("\n"))
        .replace("{{health}}", &health_lines_for_addon(config));
    std::fs::write(&quadlet_path, contents)?;
    systemctl_for_dir(&quadlet_dir, &["daemon-reload"])?;
    systemctl_for_dir(
        &quadlet_dir,
        &["enable", "--now", &format!("{}.service", unit_name)],
    )?;
    Ok(())
}

fn require_addon_image(config: &AddonConfigFile) -> Result<()> {
    if config.image.trim().is_empty() {
        anyhow::bail!("addon config must include an image");
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct AddonListEntry {
    name: String,
    kind: Option<String>,
    image: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AddonConfigFile {
    kind: Option<String>,
    image: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    volumes: Vec<String>,
    #[serde(default)]
    ports: Vec<String>,
    network: Option<String>,
    #[serde(default)]
    provision: Vec<String>,
    #[serde(default)]
    export_env: Vec<String>,
    #[serde(default)]
    bind_env: BTreeMap<String, String>,
    health_cmd: Option<String>,
    health_interval_ms: Option<u64>,
    health_timeout_ms: Option<u64>,
    health_retries: Option<u32>,
}

fn ensure_addon_dir(dir: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(())
}

fn addon_config_path(dir: &PathBuf, name: &str) -> PathBuf {
    dir.join(format!("{}.toml", name))
}

fn list_addon_configs(dir: &PathBuf) -> Result<Vec<AddonListEntry>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown")
            .to_string();
        let cfg = load_addon_config_file(&path)?;
        entries.push(AddonListEntry {
            name,
            kind: cfg.kind,
            image: cfg.image,
        });
    }
    Ok(entries)
}

fn load_addon_config_by_name(dir: &PathBuf, name: &str) -> Result<AddonConfigFile> {
    let path = addon_config_path(dir, name);
    load_addon_config_file(&path)
        .with_context(|| format!("addon config not found: {}", path.display()))
}

fn load_addon_config_file(path: &PathBuf) -> Result<AddonConfigFile> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read addon config at {}", path.display()))?;
    let cfg: AddonConfigFile =
        toml::from_str(&raw).with_context(|| "failed to parse addon config")?;
    Ok(cfg)
}

fn write_addon_config_file(path: &PathBuf, cfg: &AddonConfigFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw = toml::to_string_pretty(cfg).context("failed to serialize addon config")?;
    std::fs::write(path, raw)
        .with_context(|| format!("failed to write addon config at {}", path.display()))?;
    Ok(())
}

fn addon_config_to_json(cfg: &AddonConfigFile) -> Result<String> {
    let value = serde_json::json!({
        "kind": cfg.kind,
        "image": cfg.image,
        "env": cfg.env,
        "volumes": cfg.volumes,
        "ports": cfg.ports,
        "network": cfg.network,
        "provision": cfg.provision,
        "export_env": cfg.export_env,
        "bind_env": cfg.bind_env,
        "health_cmd": cfg.health_cmd,
        "health_interval_ms": cfg.health_interval_ms,
        "health_timeout_ms": cfg.health_timeout_ms,
        "health_retries": cfg.health_retries,
    });
    Ok(value.to_string())
}

fn addon_config_from_json(config_json: &str, kind: &str) -> Result<AddonConfigFile> {
    let value: Value = serde_json::from_str(config_json).unwrap_or(Value::Null);
    let image = value
        .get("image")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let env = value
        .get("env")
        .and_then(|v| v.as_object())
        .map(json_map_to_string_map)
        .unwrap_or_default();
    let volumes = value
        .get("volumes")
        .and_then(|v| v.as_array())
        .map(json_array_to_vec)
        .unwrap_or_default();
    let ports = value
        .get("ports")
        .and_then(|v| v.as_array())
        .map(json_array_to_vec)
        .unwrap_or_default();
    let network = value
        .get("network")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let provision = value
        .get("provision")
        .and_then(|v| v.as_array())
        .map(json_array_to_vec)
        .unwrap_or_default();
    let export_env = value
        .get("export_env")
        .and_then(|v| v.as_array())
        .map(json_array_to_vec)
        .unwrap_or_default();
    let bind_env = value
        .get("bind_env")
        .and_then(|v| v.as_object())
        .map(json_map_to_string_map)
        .unwrap_or_default();
    let health_cmd = value
        .get("health_cmd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let health_interval_ms = value.get("health_interval_ms").and_then(|v| v.as_u64());
    let health_timeout_ms = value.get("health_timeout_ms").and_then(|v| v.as_u64());
    let health_retries = value
        .get("health_retries")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    Ok(AddonConfigFile {
        kind: Some(kind.to_string()),
        image,
        env,
        volumes,
        ports,
        network,
        provision,
        export_env,
        bind_env,
        health_cmd,
        health_interval_ms,
        health_timeout_ms,
        health_retries,
    })
}

fn addon_action(name: &str, action: &str) -> Result<()> {
    let unit_name = format!("deep-addon-{}", name);
    let quadlet_dir = default_quadlet_dir();
    let unit = format!("{}.service", unit_name);
    match action {
        "start" => systemctl_for_dir(&quadlet_dir, &["start", &unit])?,
        "stop" => systemctl_for_dir(&quadlet_dir, &["stop", &unit])?,
        "restart" => systemctl_for_dir(&quadlet_dir, &["restart", &unit])?,
        _ => anyhow::bail!("unknown addon action {}", action),
    }
    println!("{} addon {}", action, name);
    Ok(())
}

fn restart_app_with_bindings(storage: &mut Storage, app_row: &AppRow) -> Result<()> {
    let release_id = storage
        .current_release_id(&app_row.id)?
        .context("no current release set")?;
    let release = storage
        .get_release_by_id(&release_id)?
        .context("current release not found")?;
    let mut snapshot: crate::config::ConfigSnapshot =
        serde_json::from_str(&release.config_json).context("invalid release config")?;
    let addons = storage.addon_snapshots_for_app(&app_row.id)?;
    snapshot.addons = addons;
    apply_addon_env(&mut snapshot);
    if snapshot.deploy.quadlet_dir.is_none() {
        snapshot.deploy.quadlet_dir = Some(default_quadlet_dir());
    }
    let quadlet_dir = snapshot.deploy.quadlet_dir.clone().unwrap_or_default();
    let unit_name = crate::runtime::app_container_name(&app_row.name, &release_id);
    write_app_quadlet(
        &quadlet_dir,
        &unit_name,
        &release.image_ref,
        &snapshot,
        &app_row.name,
        &release_id,
    )?;
    systemctl_for_dir(&quadlet_dir, &["daemon-reload"])?;
    systemctl_for_dir(
        &quadlet_dir,
        &["restart", &format!("{}.service", unit_name)],
    )?;
    Ok(())
}

fn provision_addon_on_bind(
    addon: &AddonRow,
    config: &AddonConfigFile,
    app: &AppRow,
) -> Result<BTreeMap<String, String>> {
    let mut envs = config.bind_env.clone();
    let container = format!("deep-addon-{}", addon.name);
    let command_envs = run_provision_commands(&container, app, &config.provision)?;
    for (key, value) in command_envs {
        envs.insert(key, value);
    }
    let mut exported = read_container_env(&container)?;
    for key in &config.export_env {
        if let Some(value) = exported.remove(key) {
            envs.insert(key.clone(), value);
        }
    }
    Ok(envs)
}

fn run_provision_commands(
    container: &str,
    app: &AppRow,
    commands: &[String],
) -> Result<BTreeMap<String, String>> {
    let mut envs = BTreeMap::new();
    for cmd in commands {
        let output = runner::run_output(
            "podman",
            &[
                "exec",
                "-e",
                &format!("DEEP_APP={}", app.name),
                "-e",
                &format!("DEEP_APP_ID={}", app.id),
                "-e",
                &format!("DEEP_ADDON={}", container),
                container,
                "sh",
                "-lc",
                cmd,
            ],
        )
        .with_context(|| "failed to run addon provision command")?;
        if !output.status.success() {
            bail!(
                "addon provision failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some((key, value)) = line.split_once('=') {
                if !key.trim().is_empty() {
                    envs.insert(key.trim().to_string(), value.trim().to_string());
                }
            }
        }
    }
    Ok(envs)
}

fn read_container_env(container: &str) -> Result<BTreeMap<String, String>> {
    let output = runner::run_output(
        "podman",
        &["inspect", "--format", "{{json .Config.Env}}", container],
    )
    .with_context(|| "failed to read addon container env")?;
    if !output.status.success() {
        bail!("failed to inspect addon container {}", container);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let values: Vec<String> = serde_json::from_str(stdout.trim()).unwrap_or_default();
    let mut envs = BTreeMap::new();
    for entry in values {
        if let Some((key, value)) = entry.split_once('=') {
            envs.insert(key.to_string(), value.to_string());
        }
    }
    Ok(envs)
}

fn health_lines_for_addon(config: &AddonConfigFile) -> String {
    let command = match config.health_cmd.as_ref() {
        Some(cmd) if !cmd.trim().is_empty() => cmd.trim(),
        _ => return String::new(),
    };
    let interval = format_duration_ms(config.health_interval_ms.unwrap_or(1000));
    let timeout = format_duration_ms(config.health_timeout_ms.unwrap_or(1000));
    let retries = config.health_retries.unwrap_or(3);
    format!(
        "HealthCmd={}\nHealthInterval={}\nHealthTimeout={}\nHealthRetries={}",
        command, interval, timeout, retries
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
    use crate::db::{ReleaseRow, Storage};
    use crate::runner::{Runner, set_runner_for_tests};
    use std::process::{ExitStatus, Output};
    use std::sync::{Arc, Mutex, OnceLock};

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
        fn output(&self, program: &str, args: &[&str]) -> anyhow::Result<Output> {
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

    #[test]
    fn provision_and_export_env_are_merged() -> Result<()> {
        let runner = Arc::new(TestRunner::default());
        runner.add_rule(
            &["podman exec", "deep-addon-pg", "init-db"],
            0,
            "DB=app\n",
            "",
        );
        runner.add_rule(
            &["podman inspect", "deep-addon-pg"],
            0,
            "[\"HOST=127.0.0.1\",\"PORT=5432\"]",
            "",
        );
        let _guard = set_runner_for_tests(runner);

        let addon = AddonRow {
            id: "addon1".to_string(),
            name: "pg".to_string(),
            kind: "postgres".to_string(),
            config_json: "{}".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
        };
        let app = AppRow {
            id: "app1".to_string(),
            name: "app".to_string(),
            repo_path: "/tmp".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        };
        let mut bind_env = BTreeMap::new();
        bind_env.insert("STATIC".to_string(), "1".to_string());
        let cfg = AddonConfigFile {
            kind: Some("postgres".to_string()),
            image: "postgres:16".to_string(),
            env: BTreeMap::new(),
            volumes: Vec::new(),
            ports: Vec::new(),
            network: None,
            provision: vec!["init-db".to_string()],
            export_env: vec!["HOST".to_string()],
            bind_env,
            health_cmd: None,
            health_interval_ms: None,
            health_timeout_ms: None,
            health_retries: None,
        };

        let envs = provision_addon_on_bind(&addon, &cfg, &app)?;
        assert_eq!(envs.get("STATIC"), Some(&"1".to_string()));
        assert_eq!(envs.get("DB"), Some(&"app".to_string()));
        assert_eq!(envs.get("HOST"), Some(&"127.0.0.1".to_string()));
        assert!(envs.get("PORT").is_none());
        Ok(())
    }

    #[derive(Default)]
    struct RecordingRunner {
        commands: Mutex<Vec<String>>,
    }

    impl Runner for RecordingRunner {
        fn output(&self, program: &str, args: &[&str]) -> anyhow::Result<Output> {
            let args_joined = args.iter().copied().collect::<Vec<&str>>().join(" ");
            let cmdline = format!("{} {}", program, args_joined);
            self.commands.lock().expect("commands lock").push(cmdline);
            Ok(Output {
                status: exit_status(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    #[test]
    fn addon_quadlet_renders_env_ports_volumes_and_health() -> Result<()> {
        let temp = tempfile::TempDir::new()?;
        let home = temp.path().join("home");
        std::fs::create_dir_all(&home)?;
        let _env_guard = set_home_for_test(&home)?;

        let runner = Arc::new(RecordingRunner::default());
        let _guard = set_runner_for_tests(runner);

        let mut env = BTreeMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        let config = AddonConfigFile {
            kind: Some("redis".to_string()),
            image: "redis:7".to_string(),
            env,
            volumes: vec!["redis-data:/data".to_string()],
            ports: vec!["127.0.0.1:6379:6379".to_string()],
            network: Some("deep-net".to_string()),
            provision: Vec::new(),
            export_env: Vec::new(),
            bind_env: BTreeMap::new(),
            health_cmd: Some("redis-cli ping".to_string()),
            health_interval_ms: Some(1200),
            health_timeout_ms: Some(800),
            health_retries: Some(4),
        };

        maybe_start_addon_quadlet("cache", &config)?;

        let quadlet_dir = default_quadlet_dir();
        let quadlet_path = std::path::Path::new(&quadlet_dir).join("deep-addon-cache.container");
        let contents = std::fs::read_to_string(&quadlet_path)?;
        assert!(contents.contains("Image=redis:7"));
        assert!(contents.contains("ContainerName=deep-addon-cache"));
        assert!(contents.contains("Environment=FOO=bar"));
        assert!(contents.contains("Volume=redis-data:/data"));
        assert!(contents.contains("PublishPort=127.0.0.1:6379:6379"));
        assert!(contents.contains("Network=deep-net"));
        assert!(contents.contains("HealthCmd=redis-cli ping"));
        assert!(contents.contains("HealthInterval=1200ms"));
        assert!(contents.contains("HealthTimeout=800ms"));
        assert!(contents.contains("HealthRetries=4"));

        Ok(())
    }

    #[test]
    fn bind_restart_rewrites_quadlet_with_addon_env() -> Result<()> {
        let temp = tempfile::TempDir::new()?;
        let db_path = temp.path().join("deep.db");
        let quadlet_dir = temp.path().join("quadlets");
        std::fs::create_dir_all(&quadlet_dir)?;

        let runner = Arc::new(RecordingRunner::default());
        let _guard = set_runner_for_tests(runner);

        let mut storage = Storage::open(&db_path)?;
        let app = storage.create_app("app", temp.path().to_string_lossy().as_ref())?;

        let addon_config = serde_json::json!({
            "env": { "STATIC": "1" }
        });
        let addon = storage.create_addon("pg", "postgres", &addon_config.to_string())?;
        let binding_config = serde_json::json!({
            "env": { "DYNAMIC": "2" }
        });
        storage.bind_addon(&app.id, &addon.id, &binding_config.to_string())?;

        let snapshot = crate::config::ConfigSnapshot {
            env: Default::default(),
            port: 15432,
            domains: vec!["app.example.com".to_string()],
            addons: Vec::new(),
            healthcheck: crate::config::HealthcheckConfig::default(),
            deploy: crate::config::DeployConfig {
                image: Some("ghcr.io/me/app:latest".to_string()),
                image_prefix: None,
                tag_strategy: None,
                git_ref: None,
                quadlet_dir: Some(quadlet_dir.to_string_lossy().to_string()),
                image_template: None,
                retain: 5,
            },
        };
        let release = ReleaseRow {
            id: "r1".to_string(),
            app_id: app.id.clone(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            git_sha: "deadbeef".to_string(),
            image_ref: "ghcr.io/me/app:latest".to_string(),
            image_digest: "ghcr.io/me/app@sha256:deadbeef".to_string(),
            config_json: serde_json::to_string(&snapshot)?,
            status: "active".to_string(),
        };
        let tx = storage.transaction()?;
        Storage::insert_release(&tx, &release)?;
        Storage::set_current_release(&tx, &app.id, &release.id)?;
        tx.commit()?;

        restart_app_with_bindings(&mut storage, &app)?;

        let quadlet_path = quadlet_dir.join("deep-app-app-r1.container");
        let contents = std::fs::read_to_string(&quadlet_path)?;
        assert!(contents.contains("Environment=STATIC=1"));
        assert!(contents.contains("Environment=DYNAMIC=2"));
        Ok(())
    }

    struct EnvGuard {
        previous: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe {
                    std::env::set_var("HOME", value);
                },
                None => unsafe {
                    std::env::remove_var("HOME");
                },
            }
        }
    }

    fn set_home_for_test(path: &std::path::Path) -> Result<EnvGuard> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = ENV_LOCK.get_or_init(|| Mutex::new(()));
        let guard = lock.lock().expect("env lock");
        let previous = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", path);
        }
        Ok(EnvGuard {
            previous,
            _lock: guard,
        })
    }
}

fn json_array_to_vec(value: &Vec<Value>) -> Vec<String> {
    value
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect()
}

fn json_map_to_string_map(map: &serde_json::Map<String, Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (key, value) in map {
        if let Some(val) = value.as_str() {
            out.insert(key.clone(), val.to_string());
        }
    }
    out
}
