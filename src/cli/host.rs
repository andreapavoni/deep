use anyhow::{Context, Result, bail};
use clap::Subcommand;
use std::path::PathBuf;

use crate::db::Storage;
use crate::proxy::CaddyFile;
use crate::runtime::Runtime;
use crate::systemd::{systemctl_active_any, systemctl_any, systemctl_for_dir};

#[derive(Subcommand, Debug)]
/// Host management commands.
pub enum HostCommand {
    /// Initialize host directories, network, and Caddy quadlet
    #[command(alias = "in")]
    Init {
        #[arg(
            short = 'd',
            long,
            default_value = "/srv/deep",
            help = "Base data directory"
        )]
        data_dir: PathBuf,
        #[arg(short = 'r', long, help = "Repository directory")]
        repos_dir: Option<PathBuf>,
        #[arg(short = 'b', long, help = "SQLite database path")]
        db: Option<PathBuf>,
        #[arg(
            short = 'n',
            long,
            default_value = "deep-caddy",
            help = "Caddy service name"
        )]
        caddy_name: String,
        #[arg(
            short = 'i',
            long,
            default_value = "caddy:2-alpine",
            help = "Caddy image"
        )]
        caddy_image: String,
        #[arg(short = 'H', long, default_value_t = 80, help = "HTTP port")]
        http_port: u16,
        #[arg(short = 'S', long, default_value_t = 443, help = "HTTPS port")]
        https_port: u16,
        #[arg(short = 's', long, help = "Force system quadlets")]
        system: bool,
        #[arg(short = 'u', long, help = "Force user quadlets")]
        user: bool,
        #[arg(short = 'q', long, help = "Skip writing Caddy quadlet")]
        skip_caddy_quadlet: bool,
        #[arg(short = 'k', long, help = "Skip starting Caddy service")]
        skip_caddy_start: bool,
        #[arg(short = 'N', long, help = "Skip creating deep-net network")]
        skip_network: bool,
        #[arg(short = 'C', long, help = "Skip Caddyfile check")]
        skip_caddy_check: bool,
        #[arg(short = 'D', long, help = "Print actions without executing")]
        dry_run: bool,
    },
    /// Check host health (db, network, caddy)
    #[command(alias = "st")]
    Status,
    /// Create and start a Caddy quadlet
    #[command(alias = "cs")]
    StartCaddy {
        #[arg(
            short = 'i',
            long,
            default_value = "caddy:2-alpine",
            help = "Caddy image"
        )]
        image: String,
        #[arg(
            short = 'n',
            long,
            default_value = "deep-caddy",
            help = "Caddy service name"
        )]
        name: String,
        #[arg(
            short = 'd',
            long,
            default_value = "/srv/deep/caddy/data",
            help = "Caddy data directory"
        )]
        data_dir: PathBuf,
        #[arg(
            short = 'c',
            long,
            default_value = "/srv/deep/caddy/config",
            help = "Caddy config directory"
        )]
        config_dir: PathBuf,
        #[arg(
            short = 'q',
            long,
            default_value = "",
            help = "Quadlet directory override"
        )]
        quadlet_dir: PathBuf,
        #[arg(short = 'H', long, default_value_t = 80, help = "HTTP port")]
        http_port: u16,
        #[arg(short = 'S', long, default_value_t = 443, help = "HTTPS port")]
        https_port: u16,
        #[arg(short = 's', long, help = "Force system quadlets")]
        system: bool,
        #[arg(short = 'u', long, help = "Force user quadlets")]
        user: bool,
    },
    /// Stop the Caddy service
    #[command(alias = "ct")]
    StopCaddy {
        #[arg(
            short = 'n',
            long,
            default_value = "deep-caddy",
            help = "Caddy service name"
        )]
        name: String,
    },
    /// Restart the Caddy service
    #[command(alias = "cr")]
    RestartCaddy {
        #[arg(
            short = 'n',
            long,
            default_value = "deep-caddy",
            help = "Caddy service name"
        )]
        name: String,
    },
}

/// Handle host subcommands.
pub fn handle(storage: &mut Storage, proxy: &CaddyFile, command: HostCommand) -> Result<()> {
    match command {
        HostCommand::Init {
            data_dir,
            repos_dir,
            db,
            caddy_name,
            caddy_image,
            http_port,
            https_port,
            system,
            user,
            skip_caddy_quadlet,
            skip_caddy_start,
            skip_network,
            skip_caddy_check,
            dry_run,
        } => handle_init(
            storage,
            proxy,
            data_dir,
            repos_dir,
            db,
            caddy_name,
            caddy_image,
            http_port,
            https_port,
            system,
            user,
            skip_caddy_quadlet,
            skip_caddy_start,
            skip_network,
            skip_caddy_check,
            dry_run,
        ),
        HostCommand::Status => handle_status(storage, proxy),
        HostCommand::StartCaddy {
            image,
            name,
            data_dir,
            config_dir,
            quadlet_dir,
            http_port,
            https_port,
            system,
            user,
        } => handle_caddy_start(
            data_dir,
            config_dir,
            quadlet_dir,
            image,
            name,
            http_port,
            https_port,
            system,
            user,
        ),
        HostCommand::StopCaddy { name } => handle_caddy_stop(name),
        HostCommand::RestartCaddy { name } => handle_caddy_restart(name),
    }
}

fn handle_init(
    _storage: &mut Storage,
    proxy: &CaddyFile,
    data_dir: PathBuf,
    repos_dir: Option<PathBuf>,
    db: Option<PathBuf>,
    caddy_name: String,
    caddy_image: String,
    http_port: u16,
    https_port: u16,
    system: bool,
    user: bool,
    skip_caddy_quadlet: bool,
    skip_caddy_start: bool,
    skip_network: bool,
    skip_caddy_check: bool,
    dry_run: bool,
) -> Result<()> {
    let repos_dir = repos_dir.unwrap_or_else(|| data_dir.join("repos"));
    let db_path = db.unwrap_or_else(|| data_dir.join("deep.db"));
    let caddy_data_dir = data_dir.join("caddy").join("data");
    let caddy_config_dir = data_dir.join("caddy").join("config");
    let quadlet_dir = if skip_caddy_quadlet {
        None
    } else {
        Some(select_quadlet_dir(system, user, http_port, https_port)?)
    };

    if dry_run {
        print_host_init_plan(
            &data_dir,
            &repos_dir,
            &db_path,
            &caddy_name,
            &caddy_image,
            http_port,
            https_port,
            quadlet_dir.as_ref(),
            skip_caddy_quadlet,
            skip_caddy_start,
            skip_network,
            skip_caddy_check,
        );
        return Ok(());
    }

    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("failed to create {}", data_dir.display()))?;
    std::fs::create_dir_all(&repos_dir)
        .with_context(|| format!("failed to create {}", repos_dir.display()))?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if !db_path.exists() {
        std::fs::File::create(&db_path)
            .with_context(|| format!("failed to create {}", db_path.display()))?;
    }

    if !skip_network {
        let runtime = Runtime::detect()?;
        runtime.ensure_deep_network()?;
    }

    if !skip_caddy_quadlet {
        let quadlet_dir = quadlet_dir.expect("quadlet_dir set when not skipping");
        std::fs::create_dir_all(&caddy_data_dir)
            .with_context(|| format!("failed to create {}", caddy_data_dir.display()))?;
        std::fs::create_dir_all(&caddy_config_dir)
            .with_context(|| format!("failed to create {}", caddy_config_dir.display()))?;
        std::fs::create_dir_all(&quadlet_dir)
            .with_context(|| format!("failed to create {}", quadlet_dir.display()))?;
        write_caddy_quadlet(
            &quadlet_dir,
            &caddy_name,
            &caddy_image,
            &caddy_data_dir,
            &caddy_config_dir,
            http_port,
            https_port,
        )?;
        systemctl_for_dir(quadlet_dir.to_string_lossy().as_ref(), &["daemon-reload"])?;
        if !skip_caddy_start {
            systemctl_for_dir(
                quadlet_dir.to_string_lossy().as_ref(),
                &["enable", "--now", &format!("{}.service", caddy_name)],
            )?;
        }
    }

    if !skip_caddy_check {
        proxy
            .list_routes()
            .with_context(|| "failed to read Caddyfile")?;
    }

    println!("host initialized");
    println!("data_dir={}", data_dir.display());
    println!("repos_dir={}", repos_dir.display());
    println!("db={}", db_path.display());
    println!("caddy_name={}", caddy_name);
    Ok(())
}

fn handle_status(storage: &mut Storage, proxy: &CaddyFile) -> Result<()> {
    let db_ok = storage.ping().is_ok();
    let runtime = Runtime::detect()?;
    let net_ok = runtime.deep_network_exists();
    let caddy_ok = proxy.list_routes().is_ok() && systemctl_active_any(proxy.container_name())?;

    println!("db_ok={}", db_ok);
    println!("network_ok={}", net_ok);
    println!("caddy_ok={}", caddy_ok);

    if !db_ok {
        bail!("database check failed");
    }
    if !net_ok {
        bail!("deep-net missing");
    }
    if !caddy_ok {
        bail!("caddy service not reachable");
    }
    Ok(())
}

fn handle_caddy_start(
    data_dir: PathBuf,
    config_dir: PathBuf,
    quadlet_dir: PathBuf,
    image: String,
    name: String,
    http_port: u16,
    https_port: u16,
    system: bool,
    user: bool,
) -> Result<()> {
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("failed to create {}", data_dir.display()))?;
    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("failed to create {}", config_dir.display()))?;
    let quadlet_dir = if quadlet_dir.as_os_str().is_empty() {
        select_quadlet_dir(system, user, http_port, https_port)?
    } else {
        quadlet_dir
    };
    std::fs::create_dir_all(&quadlet_dir)
        .with_context(|| format!("failed to create {}", quadlet_dir.display()))?;
    write_caddy_quadlet(
        &quadlet_dir,
        &name,
        &image,
        &data_dir,
        &config_dir,
        http_port,
        https_port,
    )?;
    systemctl_for_dir(quadlet_dir.to_string_lossy().as_ref(), &["daemon-reload"])?;
    systemctl_for_dir(
        quadlet_dir.to_string_lossy().as_ref(),
        &["enable", "--now", &format!("{}.service", name)],
    )?;
    println!("caddy service running: {}", name);
    Ok(())
}

fn handle_caddy_stop(name: String) -> Result<()> {
    systemctl_any(&["stop", &format!("{}.service", name)])?;
    println!("caddy service stopped: {}", name);
    Ok(())
}

fn handle_caddy_restart(name: String) -> Result<()> {
    systemctl_any(&["restart", &format!("{}.service", name)])?;
    println!("caddy service restarted: {}", name);
    Ok(())
}

fn write_caddy_quadlet(
    quadlet_dir: &PathBuf,
    name: &str,
    image: &str,
    data_dir: &PathBuf,
    config_dir: &PathBuf,
    http_port: u16,
    https_port: u16,
) -> Result<()> {
    let quadlet_path = quadlet_dir.join(format!("{}.container", name));
    let template = include_str!("../../templates/caddy.container");
    let contents = template
        .replace("{{image}}", image)
        .replace("{{name}}", name)
        .replace("{{http_port}}", &http_port.to_string())
        .replace("{{https_port}}", &https_port.to_string())
        .replace("{{data_dir}}", data_dir.to_string_lossy().as_ref())
        .replace("{{config_dir}}", config_dir.to_string_lossy().as_ref());
    std::fs::write(&quadlet_path, contents)?;
    Ok(())
}

fn select_quadlet_dir(
    system: bool,
    user: bool,
    http_port: u16,
    https_port: u16,
) -> Result<PathBuf> {
    if system && user {
        bail!("choose only one of --system or --user");
    }
    let min_port = http_port.min(https_port);
    let needs_low = min_port < 1024;

    if user {
        if needs_low && !user_can_bind_low_ports(min_port) {
            bail!(
                "user quadlets cannot bind to ports <1024; use --system or set net.ipv4.ip_unprivileged_port_start=0"
            );
        }
        return user_quadlet_dir();
    }
    if system {
        return Ok(PathBuf::from("/etc/containers/systemd"));
    }
    if needs_low {
        if user_can_bind_low_ports(min_port) {
            return user_quadlet_dir();
        }
        return Ok(PathBuf::from("/etc/containers/systemd"));
    }
    user_quadlet_dir()
}

fn user_quadlet_dir() -> Result<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        return Ok(PathBuf::from(format!(
            "{}/.config/containers/systemd",
            home
        )));
    }
    bail!("HOME not set for user quadlets");
}

fn user_can_bind_low_ports(port: u16) -> bool {
    let path = "/proc/sys/net/ipv4/ip_unprivileged_port_start";
    let raw = match std::fs::read_to_string(path) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let value: u16 = match raw.trim().parse() {
        Ok(val) => val,
        Err(_) => return false,
    };
    value <= port
}

fn print_host_init_plan(
    data_dir: &PathBuf,
    repos_dir: &PathBuf,
    db_path: &PathBuf,
    caddy_name: &str,
    caddy_image: &str,
    http_port: u16,
    https_port: u16,
    quadlet_dir: Option<&PathBuf>,
    skip_caddy_quadlet: bool,
    skip_caddy_start: bool,
    skip_network: bool,
    skip_caddy_check: bool,
) {
    let caddy_data_dir = data_dir.join("caddy").join("data");
    let caddy_config_dir = data_dir.join("caddy").join("config");
    println!("dry-run: host init");
    println!("data_dir={}", data_dir.display());
    println!("repos_dir={}", repos_dir.display());
    println!("db={}", db_path.display());
    if skip_network {
        println!("would skip network creation");
    } else {
        println!("would ensure podman network deep-net");
    }
    if skip_caddy_quadlet {
        println!("would skip caddy quadlet creation");
    } else {
        let quadlet_dir = quadlet_dir.map(|dir| dir.display().to_string());
        println!("caddy_name={}", caddy_name);
        println!("caddy_image={}", caddy_image);
        println!("caddy_ports={}/{}", http_port, https_port);
        println!("caddy_data_dir={}", caddy_data_dir.display());
        println!("caddy_config_dir={}", caddy_config_dir.display());
        if let Some(dir) = quadlet_dir {
            println!("quadlet_dir={}", dir);
            println!(
                "would write quadlet: {}",
                std::path::Path::new(&dir)
                    .join(format!("{}.container", caddy_name))
                    .display()
            );
        }
        if skip_caddy_start {
            println!("would skip caddy service start");
        } else {
            println!("would enable/start caddy service");
        }
    }
    if skip_caddy_check {
        println!("would skip caddyfile check");
    } else {
        println!("would validate caddyfile accessibility");
    }
}
