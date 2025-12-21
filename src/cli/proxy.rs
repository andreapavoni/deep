use anyhow::Result;
use clap::Subcommand;

use crate::proxy::CaddyFile;

#[derive(Subcommand, Debug)]
/// Proxy-related commands.
pub enum ProxyCommand {
    /// List and validate configured routes
    #[command(alias = "st")]
    Status,
}

/// Handle proxy subcommands.
pub fn handle(proxy: &CaddyFile, command: ProxyCommand) -> Result<()> {
    match command {
        ProxyCommand::Status => {
            let routes = proxy.list_routes()?;
            if routes.is_empty() {
                println!("no routes configured");
                return Ok(());
            }
            let mut invalid = 0;
            for route in &routes {
                let hosts = if route.hosts.is_empty() {
                    "<none>".to_string()
                } else {
                    route.hosts.join(",")
                };
                let upstreams = if route.upstreams.is_empty() {
                    "<none>".to_string()
                } else {
                    route.upstreams.join(",")
                };
                println!(
                    "{}  hosts={}  upstreams={}",
                    if route.id.is_empty() {
                        "<no-id>"
                    } else {
                        route.id.as_str()
                    },
                    hosts,
                    upstreams
                );
                if route.hosts.is_empty() || route.upstreams.is_empty() {
                    invalid += 1;
                }
            }
            if invalid > 0 {
                println!("warning: {} route(s) missing hosts or upstreams", invalid);
            }
            Ok(())
        }
    }
}
