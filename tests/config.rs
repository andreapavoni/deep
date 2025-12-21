use deep::config::{AppConfig, HealthcheckKind};

#[test]
fn parse_minimal_app_config_defaults() {
    let raw = r#"
[app]
name = "myapp"
port = 3000
domains = ["example.com"]
"#;
    let cfg: AppConfig = toml::from_str(raw).expect("parse config");
    assert_eq!(cfg.app.name, "myapp");
    assert_eq!(cfg.app.port, 3000);
    assert_eq!(cfg.app.domains, vec!["example.com"]);
    assert_eq!(cfg.env.len(), 0);
    assert_eq!(cfg.healthcheck.kind, HealthcheckKind::Http);
    assert_eq!(cfg.healthcheck.path, "/");
    assert_eq!(cfg.healthcheck.retries, 10);
    assert_eq!(cfg.healthcheck.timeout_ms, 2000);
    assert_eq!(cfg.healthcheck.interval_ms, 500);
    assert!(cfg.healthcheck.command.is_none());
    assert!(cfg.deploy.image.is_none());
    assert!(cfg.deploy.git_ref.is_none());
    assert!(cfg.deploy.image_prefix.is_none());
    assert!(cfg.deploy.tag_strategy.is_none());
    assert!(cfg.deploy.quadlet_dir.is_none());
    assert!(cfg.deploy.image_template.is_none());
    assert_eq!(cfg.deploy.retain, 10);
}

#[test]
fn parse_app_config_with_healthcheck_overrides() {
    let raw = r#"
[app]
name = "myapp"
port = 8080
domains = ["app.example.com"]

[healthcheck]
kind = "tcp"
path = "/health"
retries = 3
timeout_ms = 1500
interval_ms = 250

[env]
RUST_LOG = "info"
"#;
    let cfg: AppConfig = toml::from_str(raw).expect("parse config");
    assert_eq!(cfg.app.port, 8080);
    assert_eq!(cfg.healthcheck.kind, HealthcheckKind::Tcp);
    assert_eq!(cfg.healthcheck.path, "/health");
    assert_eq!(cfg.healthcheck.retries, 3);
    assert_eq!(cfg.healthcheck.timeout_ms, 1500);
    assert_eq!(cfg.healthcheck.interval_ms, 250);
    assert!(cfg.healthcheck.command.is_none());
    assert_eq!(cfg.env.get("RUST_LOG").map(String::as_str), Some("info"));
    assert!(cfg.deploy.image.is_none());
    assert!(cfg.deploy.git_ref.is_none());
    assert!(cfg.deploy.image_prefix.is_none());
    assert!(cfg.deploy.tag_strategy.is_none());
    assert!(cfg.deploy.quadlet_dir.is_none());
    assert!(cfg.deploy.image_template.is_none());
    assert_eq!(cfg.deploy.retain, 10);
}

#[test]
fn parse_app_config_with_deploy_defaults() {
    let raw = r#"
[app]
name = "myapp"
port = 8080
domains = ["app.example.com"]

[deploy]
image = "ghcr.io/me/myapp:latest"
git_ref = "refs/heads/main"
image_prefix = "ghcr.io/me/myapp"
tag_strategy = "git_sha"
quadlet_dir = "/etc/containers/systemd"
image_template = "ghcr.io/me/{{app}}:{{sha}}"
retain = 7
"#;
    let cfg: AppConfig = toml::from_str(raw).expect("parse config");
    assert_eq!(cfg.deploy.image.as_deref(), Some("ghcr.io/me/myapp:latest"));
    assert_eq!(cfg.deploy.git_ref.as_deref(), Some("refs/heads/main"));
    assert_eq!(cfg.deploy.image_prefix.as_deref(), Some("ghcr.io/me/myapp"));
    assert_eq!(cfg.deploy.tag_strategy.as_deref(), Some("git_sha"));
    assert_eq!(
        cfg.deploy.quadlet_dir.as_deref(),
        Some("/etc/containers/systemd")
    );
    assert_eq!(
        cfg.deploy.image_template.as_deref(),
        Some("ghcr.io/me/{{app}}:{{sha}}")
    );
    assert_eq!(cfg.deploy.retain, 7);
}
