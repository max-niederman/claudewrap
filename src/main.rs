mod cli;
mod config;
mod resolve;
mod sandbox;
mod sockets;
#[allow(dead_code)]
mod ssh_protocol;
mod ssh_proxy;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use tracing::info;

use cli::Cli;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("claudewrap: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    if cli.verbose {
                        "claudewrap=debug".into()
                    } else {
                        "claudewrap=warn".into()
                    }
                }),
        )
        .without_time()
        .init();

    let config = resolve::resolve(&cli)?;

    if cli.verbose {
        info!(scopes = ?config.active_scopes, "active scopes");
        info!(paths = ?config.write_paths, "write paths");
        info!(wayland = config.wayland, pipewire = config.pipewire, dbus = ?config.dbus, "sockets");
        info!(agent = config.ssh.agent, signing = config.ssh.allow_signing, ssh = config.ssh.allow_ssh, hosts = ?config.ssh.allow_hosts, "ssh");
        info!(command = config.command, args = ?config.cmd_args, "command");
    }

    // Create temp dir
    let pid = std::process::id();
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| "/tmp".into());
    let temp_dir = PathBuf::from(&runtime_dir).join(format!("claudewrap-{pid}"));
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("creating temp dir {}", temp_dir.display()))?;

    // Start SSH proxy if agent is enabled
    let proxy_sock = if config.ssh.agent {
        if let Ok(real_sock) = std::env::var("SSH_AUTH_SOCK") {
            let (sock_path, _handle) = ssh_proxy::start_proxy(&temp_dir, &real_sock)?;
            info!("SSH proxy listening at {}", sock_path.display());
            Some(sock_path)
        } else {
            None
        }
    } else {
        None
    };

    // Generate SSH wrapper if SSH is restricted
    let wrapper_bin_dir = if !config.ssh.allow_ssh && !config.ssh_allow_all {
        Some(generate_ssh_wrapper(&temp_dir, &config)?)
    } else {
        None
    };

    // Build bwrap command
    let cmd = sandbox::build_command(
        &config,
        proxy_sock.as_deref(),
        wrapper_bin_dir.as_deref(),
    );

    if config.dry_run {
        println!("{}", sandbox::format_command(&cmd));
        cleanup(&temp_dir);
        return Ok(ExitCode::SUCCESS);
    }

    // Spawn
    let mut child = std::process::Command::from(cmd)
        .spawn()
        .context("spawning bwrap")?;

    // Forward signals
    let child_id = child.id();
    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    std::thread::spawn(move || {
        for sig in signals.forever() {
            unsafe {
                libc::kill(child_id as i32, sig);
            }
        }
    });

    let status = child.wait().context("waiting for bwrap")?;

    cleanup(&temp_dir);

    Ok(ExitCode::from(
        status.code().unwrap_or(1) as u8,
    ))
}

fn generate_ssh_wrapper(temp_dir: &Path, config: &resolve::ResolvedConfig) -> Result<PathBuf> {
    let bin_dir = temp_dir.join("bin");
    fs::create_dir_all(&bin_dir)?;

    // Find the real ssh binary path
    let real_ssh = which_ssh().unwrap_or_else(|| "/usr/bin/ssh".into());

    // Build host allowlist
    let mut allowed_hosts: Vec<&str> = config
        .ssh
        .allow_hosts
        .iter()
        .map(|s| s.as_str())
        .collect();
    for h in &config.extra_ssh_hosts {
        allowed_hosts.push(h.as_str());
    }

    let host_check = if allowed_hosts.is_empty() {
        String::new()
    } else {
        let patterns: Vec<String> = allowed_hosts
            .iter()
            .map(|h| format!("    \"{h}\") ;;"))
            .collect();
        format!(
            r#"
case "$host" in
{patterns}
    *) echo "claudewrap: SSH to '$host' not allowed" >&2; exit 1 ;;
esac
"#,
            patterns = patterns.join("\n")
        )
    };

    let wrapper = format!(
        r##"#!/bin/sh
# claudewrap SSH wrapper — restricts SSH usage to git transport only

# Strip dangerous flags, collect clean args
args=()
skip_next=false
for arg in "$@"; do
    if $skip_next; then
        skip_next=false
        continue
    fi
    case "$arg" in
        -A|-L|-R|-D|-W|-N) continue ;;
        -o|-p|-l|-i|-F) skip_next=true; args+=("$arg") ;;
        *) args+=("$arg") ;;
    esac
done

# Find the host: first non-flag argument after stripping
host=""
for arg in "${{args[@]}}"; do
    case "$arg" in
        -*) ;;
        *)
            if [ -z "$host" ]; then
                host="$arg"
            fi
            ;;
    esac
done
# Strip user@ prefix if present
host="${{host#*@}}"
{host_check}
exec {real_ssh} "${{args[@]}}"
"##,
        real_ssh = real_ssh.display(),
    );

    let wrapper_path = bin_dir.join("ssh");
    let mut f = fs::File::create(&wrapper_path)?;
    f.write_all(wrapper.as_bytes())?;
    f.set_permissions(fs::Permissions::from_mode(0o755))?;

    Ok(bin_dir)
}

fn which_ssh() -> Option<PathBuf> {
    std::env::var("PATH")
        .ok()?
        .split(':')
        .map(|dir| PathBuf::from(dir).join("ssh"))
        .find(|p| p.is_file())
}

fn cleanup(temp_dir: &Path) {
    let _ = fs::remove_dir_all(temp_dir);
}
