//! App configuration and deploy defaults loaded from app.toml.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
/// Top-level app.toml representation.
pub struct AppConfig {
    pub app: AppSection,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub healthcheck: HealthcheckConfig,
    #[serde(default)]
    pub deploy: DeployConfig,
}

#[derive(Debug, Deserialize)]
/// Basic app metadata and routing configuration.
pub struct AppSection {
    pub name: String,
    pub port: u16,
    #[serde(default)]
    pub domains: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
/// Immutable config snapshot saved with each release.
pub struct ConfigSnapshot {
    pub env: BTreeMap<String, String>,
    pub port: u16,
    pub domains: Vec<String>,
    pub addons: Vec<AddonSnapshot>,
    pub healthcheck: HealthcheckConfig,
    pub deploy: DeployConfig,
}

#[derive(Debug, Serialize, Deserialize)]
/// Addon config snapshot embedded in a release.
pub struct AddonSnapshot {
    pub name: String,
    pub kind: String,
    pub config: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
/// Supported healthcheck modes.
pub enum HealthcheckKind {
    Http,
    Tcp,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
/// Healthcheck settings resolved at deploy time.
pub struct HealthcheckConfig {
    #[serde(default = "default_health_kind")]
    pub kind: HealthcheckKind,
    #[serde(default = "default_health_path")]
    pub path: String,
    #[serde(default = "default_health_retries")]
    pub retries: u32,
    #[serde(default = "default_health_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_health_interval_ms")]
    pub interval_ms: u64,
    pub command: Option<String>,
}

impl Default for HealthcheckConfig {
    fn default() -> Self {
        Self {
            kind: default_health_kind(),
            path: default_health_path(),
            retries: default_health_retries(),
            timeout_ms: default_health_timeout_ms(),
            interval_ms: default_health_interval_ms(),
            command: None,
        }
    }
}

impl AppConfig {
    /// Convert the current config into a release snapshot.
    pub fn to_snapshot(&self, addons: Vec<AddonSnapshot>) -> ConfigSnapshot {
        ConfigSnapshot {
            env: self.env.clone(),
            port: self.app.port,
            domains: self.app.domains.clone(),
            addons,
            healthcheck: self.healthcheck.clone(),
            deploy: self.deploy.clone(),
        }
    }
}

/// Load app.toml from disk.
pub fn load_app_config(path: &Path) -> Result<AppConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read app config at {}", path.display()))?;
    let cfg: AppConfig = toml::from_str(&raw).with_context(|| "failed to parse app.toml")?;
    Ok(cfg)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
/// Deploy defaults for a given app.
pub struct DeployConfig {
    pub image: Option<String>,
    pub image_prefix: Option<String>,
    pub tag_strategy: Option<String>,
    pub git_ref: Option<String>,
    pub quadlet_dir: Option<String>,
    pub image_template: Option<String>,
    #[serde(default = "default_deploy_retain")]
    pub retain: u32,
}

impl Default for DeployConfig {
    fn default() -> Self {
        Self {
            image: None,
            image_prefix: None,
            tag_strategy: None,
            git_ref: None,
            quadlet_dir: None,
            image_template: None,
            retain: default_deploy_retain(),
        }
    }
}

fn default_health_kind() -> HealthcheckKind {
    HealthcheckKind::Http
}

fn default_health_path() -> String {
    "/".to_string()
}

fn default_health_retries() -> u32 {
    10
}

fn default_health_timeout_ms() -> u64 {
    2_000
}

fn default_health_interval_ms() -> u64 {
    500
}

fn default_deploy_retain() -> u32 {
    10
}
