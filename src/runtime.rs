//! Podman CLI runtime helpers for image and container operations.

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use std::net::{SocketAddr, TcpStream};
use std::process::Output;
use std::time::Duration;

use crate::config::HealthcheckKind;
use crate::runner;

const NETWORK_NAME: &str = "deep-net";

#[derive(Debug, Clone)]
/// Podman runtime wrapper.
pub struct Runtime {
    engine: &'static str,
}

impl Runtime {
    /// Detect the runtime (Podman only).
    pub fn detect() -> Result<Self> {
        if runner::command_exists("podman") {
            return Ok(Self { engine: "podman" });
        }
        bail!("podman not found on PATH")
    }

    /// Pull an image and return its resolved digest.
    pub fn pull_image(&self, image_ref: &str) -> Result<String> {
        self.run(&["pull", image_ref])?;
        let digest = self.run_capture(&[
            "image",
            "inspect",
            "--format",
            "{{index .RepoDigests 0}}",
            image_ref,
        ])?;
        let digest = digest.trim();
        if digest.is_empty() || digest == "<no value>" {
            return Ok(image_ref.to_string());
        }
        Ok(digest.to_string())
    }

    /// Perform an HTTP healthcheck against a container.
    pub fn healthcheck_http(
        &self,
        container_name: &str,
        port: u16,
        path: &str,
        timeout: Duration,
    ) -> Result<()> {
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            let normalized = if path.starts_with('/') {
                path.to_string()
            } else {
                format!("/{}", path)
            };
            let ip = self.container_ip(container_name)?;
            format!("http://{}:{}{}", ip, port, normalized)
        };
        let client = Client::builder().timeout(timeout).build()?;
        let response = client.get(&url).send().context("http request failed")?;
        if !response.status().is_success() {
            bail!("http healthcheck failed with status {}", response.status());
        }
        Ok(())
    }

    /// Perform a TCP healthcheck against a container.
    pub fn healthcheck_tcp(
        &self,
        container_name: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<()> {
        let ip = self.container_ip(container_name)?;
        let addr: SocketAddr = format!("{}:{}", ip, port)
            .parse()
            .context("invalid tcp address")?;
        TcpStream::connect_timeout(&addr, timeout).context("tcp connect failed")?;
        Ok(())
    }

    /// Perform a healthcheck based on a config struct with retries.
    pub fn healthcheck_with_config(
        &self,
        container_name: &str,
        port: u16,
        config: &crate::config::HealthcheckConfig,
    ) -> Result<()> {
        let timeout = Duration::from_millis(config.timeout_ms.max(100));
        let retries = config.retries.max(1);
        let interval = std::time::Duration::from_millis(config.interval_ms.max(50));
        match config.kind {
            HealthcheckKind::Http => {
                self.retry_healthcheck_with(container_name, retries, interval, timeout, |timeout| {
                    self.healthcheck_http(container_name, port, &config.path, timeout)
                })
            }
            HealthcheckKind::Tcp => {
                self.retry_healthcheck_with(container_name, retries, interval, timeout, |timeout| {
                    self.healthcheck_tcp(container_name, port, timeout)
                })
            }
        }
    }

    /// Tail logs for a container.
    pub fn logs(&self, container_name: &str, follow: bool) -> Result<()> {
        let mut args = vec!["logs"];
        if follow {
            args.push("-f");
        }
        args.push(container_name);
        let status =
            runner::run_status(self.engine, &args).with_context(|| "failed to run logs command")?;
        if status.success() {
            Ok(())
        } else {
            bail!("logs command failed with status {}", status)
        }
    }

    fn container_ip(&self, name: &str) -> Result<String> {
        let output = self.run_capture(&[
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            name,
        ])?;
        let ip = output.trim();
        if ip.is_empty() {
            bail!("container {} has no IP address", name);
        }
        Ok(ip.to_string())
    }

    fn retry_healthcheck_with<F>(
        &self,
        _container_name: &str,
        retries: u32,
        interval: std::time::Duration,
        timeout: Duration,
        mut attempt: F,
    ) -> Result<()>
    where
        F: FnMut(Duration) -> Result<()>,
    {
        let mut last_err: Option<anyhow::Error> = None;
        for idx in 0..retries {
            match attempt(timeout) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    last_err = Some(err);
                    if idx + 1 < retries {
                        std::thread::sleep(interval);
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("healthcheck failed")))
    }

    fn ensure_network(&self) -> Result<()> {
        if self.run(&["network", "inspect", NETWORK_NAME]).is_ok() {
            return Ok(());
        }
        self.run(&["network", "create", NETWORK_NAME])?;
        Ok(())
    }

    /// Ensure the shared deep-net network exists.
    pub fn ensure_deep_network(&self) -> Result<()> {
        self.ensure_network()
    }

    /// Check whether the deep-net network exists.
    pub fn deep_network_exists(&self) -> bool {
        self.run(&["network", "inspect", NETWORK_NAME]).is_ok()
    }

    fn run(&self, args: &[&str]) -> Result<()> {
        let output = runner::run_output(self.engine, args)?;
        if output.status.success() {
            return Ok(());
        }
        bail!(command_error(&output))
    }

    fn run_capture(&self, args: &[&str]) -> Result<String> {
        let output = runner::run_output(self.engine, args)?;
        if !output.status.success() {
            bail!(command_error(&output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// Generate an app container name based on app name and release id.
pub fn app_container_name(app_name: &str, release_id: &str) -> String {
    format!("deep-app-{}-{}", app_name, release_id)
}

fn command_error(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!(
        "command failed: stdout={} stderr={}",
        stdout.trim(),
        stderr.trim()
    )
}
