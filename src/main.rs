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

    // Validate host SSH agent if enabled
    let ssh_info = if config.ssh_agent {
        if config.ssh_keys.is_empty() {
            bail!(
                "ssh.agent is enabled but no keys configured\n\
                 Add fingerprints to [ssh] keys in .claude/wrap.toml or pass --ssh-key SHA256:..."
            );
        }
        let info = validate_host_agent(&config.ssh_keys)?;
        info!("using host SSH agent at {}", info.sock.display());
        Some(info)
    } else {
        None
    };

    // Build bwrap command
    let cmd = sandbox::build_command(&config, ssh_info.as_ref());

    if config.dry_run {
        println!("{}", sandbox::format_command(&cmd));
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

    Ok(ExitCode::from(
        status.code().unwrap_or(1) as u8,
    ))
}

pub struct SshAgentInfo {
    pub sock: PathBuf,
    /// Public key string for the first matched key (used for git signing)
    pub signing_key: String,
}

/// Validate the host SSH agent has all requested keys.
/// Returns socket path and the public key of the first matched key (for git signing).
fn validate_host_agent(fingerprints: &[String]) -> Result<SshAgentInfo> {
    let sock = std::env::var("SSH_AUTH_SOCK")
        .context("SSH_AUTH_SOCK is not set — is an ssh-agent running?")?;
    let sock_path = PathBuf::from(&sock);

    // List keys with SHA256 fingerprints
    let output = std::process::Command::new("ssh-add")
        .args(["-l", "-E", "sha256"])
        .env("SSH_AUTH_SOCK", &sock)
        .output()
        .context("running ssh-add -l")?;

    if !output.status.success() {
        bail!(
            "ssh-add -l failed (is your agent running?): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let listing = String::from_utf8_lossy(&output.stdout);

    // Verify all fingerprints appear in the listing
    for fp in fingerprints {
        if !listing.lines().any(|line| line.contains(fp.as_str())) {
            bail!(
                "key {fp} not found in SSH agent\n\
                 Available keys:\n{listing}\
                 Add the key with: ssh-add <path-to-key>"
            );
        }
    }

    // Get full public keys to find the matching one for git signing.
    // ssh-add -l and -L output lines in the same order, so we correlate by index.
    let pub_output = std::process::Command::new("ssh-add")
        .args(["-L"])
        .env("SSH_AUTH_SOCK", &sock)
        .output()
        .context("running ssh-add -L")?;

    if !pub_output.status.success() {
        bail!("ssh-add -L failed: {}", String::from_utf8_lossy(&pub_output.stderr).trim());
    }

    let pub_stdout = String::from_utf8_lossy(&pub_output.stdout);
    let pub_lines: Vec<&str> = pub_stdout.lines().collect();
    let fingerprint_lines: Vec<&str> = listing.lines().collect();

    // Use the first configured fingerprint for git signing
    let signing_key = fingerprint_lines
        .iter()
        .zip(pub_lines.iter())
        .find(|(fp_line, _)| fp_line.contains(fingerprints[0].as_str()))
        .map(|(_, pub_line)| pub_line.to_string())
        .ok_or_else(|| {
            anyhow::anyhow!("could not find public key for fingerprint {}", fingerprints[0])
        })?;

    Ok(SshAgentInfo {
        sock: sock_path,
        signing_key,
    })
}
