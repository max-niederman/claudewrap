mod agent_proxy;
mod cli;
mod config;
mod resolve;
mod sandbox;
mod sockets;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::Parser;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use tracing::{debug, info};

use cli::Cli;

/// Compile-time path to bwrap, set via env var during build.
/// Falls back to bare name (PATH lookup) when not set.
pub const BWRAP: &str = match option_env!("BWRAP_PATH") {
    Some(p) => p,
    None => "bwrap",
};

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
                        "claudewrap=info".into()
                    }
                }),
        )
        .without_time()
        .init();

    let config = resolve::resolve(&cli)?;

    if cli.verbose {
        debug!(scopes = ?config.active_scopes, "active scopes");
        debug!(paths = ?config.write_paths, "write paths");
        debug!(wayland = config.wayland, pipewire = config.pipewire, dbus = ?config.dbus, "sockets");
        debug!(command = config.command, args = ?config.cmd_args, "command");
    }

    // Start SSH agent filtering proxy if enabled
    let mut proxy_sock: Option<PathBuf> = None;
    let ssh_info = if config.ssh_agent {
        if config.ssh_keys.is_empty() {
            bail!(
                "ssh.agent is enabled but no keys configured\n\
                 Add fingerprints to [ssh] keys in .claude/wrap.toml or pass --ssh-key SHA256:..."
            );
        }
        let info = setup_agent_proxy(&config.ssh_keys)?;
        info!("SSH agent proxy listening at {}", info.sock.display());
        proxy_sock = Some(info.sock.clone());
        Some(info)
    } else {
        None
    };

    // Build bwrap command
    let cmd = sandbox::build_command(&config, ssh_info.as_ref());

    if config.dry_run {
        println!("{}", sandbox::format_command(&cmd));
        cleanup(proxy_sock.as_deref());
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

    cleanup(proxy_sock.as_deref());

    Ok(ExitCode::from(
        status.code().unwrap_or(1) as u8,
    ))
}

pub struct SshAgentInfo {
    pub sock: PathBuf,
    /// Public key string for the first matched key (used for git signing)
    pub signing_key: String,
}

/// Validate the host agent has the configured keys, then start a filtering
/// proxy that only exposes those keys.
fn setup_agent_proxy(fingerprints: &[String]) -> Result<SshAgentInfo> {
    let host_sock = std::env::var("SSH_AUTH_SOCK")
        .context("SSH_AUTH_SOCK is not set — is an ssh-agent running?")?;
    let host_sock_path = PathBuf::from(&host_sock);

    // List keys with SHA256 fingerprints
    let output = std::process::Command::new("ssh-add")
        .args(["-l", "-E", "sha256"])
        .env("SSH_AUTH_SOCK", &host_sock)
        .output()
        .context("running ssh-add -l")?;

    if !output.status.success() {
        bail!(
            "ssh-add -l failed (is your agent running?): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let listing = String::from_utf8_lossy(&output.stdout);

    for fp in fingerprints {
        if !listing.lines().any(|line| line.contains(fp.as_str())) {
            bail!(
                "key {fp} not found in SSH agent\n\
                 Available keys:\n{listing}\
                 Add the key with: ssh-add <path-to-key>"
            );
        }
    }

    // Get full public keys — correlate by index with fingerprint listing
    let pub_output = std::process::Command::new("ssh-add")
        .args(["-L"])
        .env("SSH_AUTH_SOCK", &host_sock)
        .output()
        .context("running ssh-add -L")?;

    if !pub_output.status.success() {
        bail!("ssh-add -L failed: {}", String::from_utf8_lossy(&pub_output.stderr).trim());
    }

    let pub_stdout = String::from_utf8_lossy(&pub_output.stdout);
    let pub_lines: Vec<&str> = pub_stdout.lines().collect();
    let fingerprint_lines: Vec<&str> = listing.lines().collect();

    // Collect key blobs and signing key for allowed fingerprints
    let mut allowed_key_blobs: Vec<Vec<u8>> = Vec::new();
    let mut signing_key: Option<String> = None;

    for fp in fingerprints {
        let (_, pub_line) = fingerprint_lines
            .iter()
            .zip(pub_lines.iter())
            .find(|(fp_line, _)| fp_line.contains(fp.as_str()))
            .ok_or_else(|| anyhow::anyhow!("could not find public key for {fp}"))?;

        // Public key line: "ssh-ed25519 AAAA... comment"
        // Decode the base64 portion to get the wire-format key blob
        let parts: Vec<&str> = pub_line.split_whitespace().collect();
        if parts.len() < 2 {
            bail!("malformed public key line for {fp}");
        }

        use base64::Engine;
        let key_blob = base64::engine::general_purpose::STANDARD
            .decode(parts[1])
            .with_context(|| format!("decoding public key for {fp}"))?;

        allowed_key_blobs.push(key_blob);
        if signing_key.is_none() {
            signing_key = Some(pub_line.to_string());
        }
    }

    let signing_key = signing_key.unwrap();

    // Start the filtering proxy
    let proxy_sock = agent_proxy::start(&host_sock_path, allowed_key_blobs)
        .context("starting SSH agent proxy")?;

    Ok(SshAgentInfo {
        sock: proxy_sock,
        signing_key,
    })
}

fn cleanup(proxy_sock: Option<&std::path::Path>) {
    if let Some(sock) = proxy_sock {
        let _ = std::fs::remove_file(sock);
        if let Some(parent) = sock.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}
