//! File-based Caddy routing updates and route inspection.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::PathBuf;

use crate::config::ConfigSnapshot;
use crate::runtime::app_container_name;
use crate::systemd::systemctl_any;

#[derive(Debug, Clone)]
/// Caddyfile-based proxy controller.
pub struct CaddyFile {
    host_path: PathBuf,
    container_name: String,
}

#[derive(Debug)]
/// Parsed route information from a Caddyfile.
pub struct RouteStatus {
    pub id: String,
    pub hosts: Vec<String>,
    pub upstreams: Vec<String>,
}

impl CaddyFile {
    /// Create a new Caddyfile controller.
    pub fn new(host_path: PathBuf, container_name: String) -> Self {
        Self {
            host_path,
            container_name,
        }
    }

    /// Get the configured Caddy service/container name.
    pub fn container_name(&self) -> &str {
        &self.container_name
    }

    /// Upsert a route for an app and reload Caddy with rollback on failure.
    pub fn upsert_route(
        &self,
        app_name: &str,
        release_id: &str,
        snapshot: &ConfigSnapshot,
    ) -> Result<()> {
        if snapshot.domains.is_empty() {
            bail!("no domains configured for app; cannot update proxy route");
        }
        let upstream = format!(
            "{}:{}",
            app_container_name(app_name, release_id),
            snapshot.port
        );
        let mut contents = String::new();
        if self.host_path.exists() {
            contents = fs::read_to_string(&self.host_path).with_context(|| {
                format!("failed to read caddyfile at {}", self.host_path.display())
            })?;
        }
        let updated = upsert_caddyfile_block(&contents, app_name, &snapshot.domains, &upstream);
        if let Some(parent) = self.host_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let backup_path = self.host_path.with_extension("bak");
        fs::write(&backup_path, &contents).with_context(|| {
            format!(
                "failed to write caddyfile backup at {}",
                backup_path.display()
            )
        })?;
        fs::write(&self.host_path, updated).with_context(|| {
            format!("failed to write caddyfile at {}", self.host_path.display())
        })?;
        if let Err(err) = self.reload() {
            fs::write(&self.host_path, &contents).with_context(|| {
                format!(
                    "failed to restore caddyfile at {}",
                    self.host_path.display()
                )
            })?;
            if let Err(rollback_err) = self.reload() {
                bail!(
                    "caddy reload failed: {}; rollback reload failed: {}",
                    err,
                    rollback_err
                );
            }
            bail!("caddy reload failed; caddyfile restored: {}", err);
        }
        Ok(())
    }

    /// List routes parsed from the Caddyfile.
    pub fn list_routes(&self) -> Result<Vec<RouteStatus>> {
        if !self.host_path.exists() {
            return Ok(Vec::new());
        }
        let contents = fs::read_to_string(&self.host_path)
            .with_context(|| format!("failed to read caddyfile at {}", self.host_path.display()))?;
        Ok(parse_caddyfile_routes(&contents))
    }

    /// Reload Caddy via systemd.
    pub fn reload(&self) -> Result<()> {
        systemctl_any(&["reload", &format!("{}.service", self.container_name)])
    }
}

fn upsert_caddyfile_block(contents: &str, app: &str, domains: &[String], upstream: &str) -> String {
    let start_marker = format!("# deep:app:{}", app);
    let end_marker = "# deep:end";
    let block = format!(
        "{start}\n{hosts} {{\n    reverse_proxy {upstream}\n}}\n{end}\n",
        start = start_marker,
        hosts = domains.join(", "),
        upstream = upstream,
        end = end_marker
    );

    let mut lines = Vec::new();
    let mut in_block = false;
    for line in contents.lines() {
        if line.trim() == start_marker {
            in_block = true;
            continue;
        }
        if in_block && line.trim() == end_marker {
            in_block = false;
            continue;
        }
        if !in_block {
            lines.push(line);
        }
    }

    let mut output = lines.join("\n");
    if !output.ends_with('\n') && !output.is_empty() {
        output.push('\n');
    }
    output.push_str(&block);
    output
}

fn parse_caddyfile_routes(contents: &str) -> Vec<RouteStatus> {
    let mut routes = Vec::new();
    let mut current: Option<RouteStatus> = None;
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# deep:app:") {
            if let Some(route) = current.take() {
                routes.push(route);
            }
            current = Some(RouteStatus {
                id: format!("deep-app-{}", rest),
                hosts: Vec::new(),
                upstreams: Vec::new(),
            });
            continue;
        }
        if trimmed == "# deep:end" {
            if let Some(route) = current.take() {
                routes.push(route);
            }
            continue;
        }
        if let Some(route) = current.as_mut() {
            if trimmed.ends_with('{') {
                let hosts = trimmed.trim_end_matches('{').trim();
                if !hosts.is_empty() {
                    route.hosts = hosts.split(',').map(|h| h.trim().to_string()).collect();
                }
            } else if let Some(rest) = trimmed.strip_prefix("reverse_proxy ") {
                route.upstreams = vec![rest.trim().to_string()];
            }
        }
    }
    if let Some(route) = current.take() {
        routes.push(route);
    }
    routes
}

#[cfg(test)]
mod public_tests {
    use super::*;

    #[test]
    fn parse_routes_from_markers() {
        let contents = r#"
# deep:app:app
app.example.com {
    reverse_proxy deep-app-app-r1:3000
}
# deep:end
"#;
        let routes = parse_caddyfile_routes(contents);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].hosts, vec!["app.example.com"]);
        assert_eq!(routes[0].upstreams, vec!["deep-app-app-r1:3000"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_replaces_existing_block() {
        let contents = r#"
# deep:app:app
old.example.com {
    reverse_proxy deep-app-app-old:3000
}
# deep:end
"#;
        let updated = upsert_caddyfile_block(
            contents,
            "app",
            &[String::from("new.example.com")],
            "deep-app-app-new:3000",
        );
        assert!(updated.contains("new.example.com"));
        assert!(updated.contains("deep-app-app-new:3000"));
        assert!(!updated.contains("old.example.com"));
    }
}
