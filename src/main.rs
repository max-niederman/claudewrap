mod cli;
mod config;
mod resolve;
mod sandbox;
mod sockets;

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::Parser;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use tracing::{debug, info};

use cli::Cli;

/// Compile-time paths to external binaries, set via env vars during build.
/// Falls back to bare names (PATH lookup) when not set.
pub const BWRAP: &str = match option_env!("BWRAP_PATH") {
    Some(p) => p,
    None => "bwrap",
};
pub const SSH_ADD: &str = match option_env!("SSH_ADD_PATH") {
    Some(p) => p,
    None => "ssh-add",
};
pub const SSH_AGENT_FILTER: &str = match option_env!("SSH_AGENT_FILTER_PATH") {
    Some(p) => p,
    None => "ssh-agent-filter",
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

    // Validate host SSH agent and start filtered proxy if enabled
    let mut filter_pid: Option<u32> = None;
    let mut filter_dir: Option<PathBuf> = None;
    let ssh_info = if config.ssh_agent {
        if config.ssh_keys.is_empty() {
            bail!(
                "ssh.agent is enabled but no keys configured\n\
                 Add fingerprints to [ssh] keys in .claude/wrap.toml or pass --ssh-key SHA256:..."
            );
        }
        let agent = validate_and_filter_agent(&config.ssh_keys)?;
        info!("ssh-agent-filter started (pid {}) at {}", agent.filter_pid, agent.sock.display());
        filter_pid = Some(agent.filter_pid);
        filter_dir = Some(agent.filter_dir.clone());
        Some(SshAgentInfo {
            sock: agent.sock,
            signing_key: agent.signing_key,
        })
    } else {
        None
    };

    // Build bwrap command
    let cmd = sandbox::build_command(&config, ssh_info.as_ref());

    if config.dry_run {
        println!("{}", sandbox::format_command(&cmd));
        cleanup(filter_pid, filter_dir.as_deref());
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

    cleanup(filter_pid, filter_dir.as_deref());

    Ok(ExitCode::from(
        status.code().unwrap_or(1) as u8,
    ))
}

pub struct SshAgentInfo {
    pub sock: PathBuf,
    /// Public key string for the first matched key (used for git signing)
    pub signing_key: String,
}

struct FilteredAgent {
    sock: PathBuf,
    signing_key: String,
    filter_pid: u32,
    filter_dir: PathBuf,
}

/// Validate the host SSH agent has all requested keys, then start
/// ssh-agent-filter to proxy only those keys.
fn validate_and_filter_agent(fingerprints: &[String]) -> Result<FilteredAgent> {
    let host_sock = std::env::var("SSH_AUTH_SOCK")
        .context("SSH_AUTH_SOCK is not set — is an ssh-agent running?")?;

    // List keys with SHA256 fingerprints
    let output = std::process::Command::new(SSH_ADD)
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

    // Get full public keys — ssh-add -l and -L output in the same order
    let pub_output = std::process::Command::new(SSH_ADD)
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

    // Collect the base64 pubkey portion for each matching fingerprint
    let mut allowed_keys: Vec<String> = Vec::new();
    let mut signing_key: Option<String> = None;

    for fp in fingerprints {
        let (_, pub_line) = fingerprint_lines
            .iter()
            .zip(pub_lines.iter())
            .find(|(fp_line, _)| fp_line.contains(fp.as_str()))
            .ok_or_else(|| anyhow::anyhow!("could not find public key for {fp}"))?;

        // Public key line format: "ssh-ed25519 AAAA... comment"
        // Extract the base64 portion (second field)
        let base64_key = pub_line
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("malformed public key line for {fp}"))?;

        allowed_keys.push(base64_key.to_string());
        if signing_key.is_none() {
            signing_key = Some(pub_line.to_string());
        }
    }

    let signing_key = signing_key.unwrap();

    // Create a temp directory for ssh-agent-filter's socket.
    // It creates the socket as cwd/agent.<pid>.
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let filter_dir = PathBuf::from(&runtime_dir).join("claudewrap");
    fs::create_dir_all(&filter_dir)
        .with_context(|| format!("creating filter dir {}", filter_dir.display()))?;

    // Start ssh-agent-filter with -k for each allowed key
    let mut filter_cmd = std::process::Command::new(SSH_AGENT_FILTER);
    for key in &allowed_keys {
        filter_cmd.arg("-k").arg(key);
    }
    filter_cmd.current_dir(&filter_dir);
    filter_cmd.env("SSH_AUTH_SOCK", &host_sock);

    let filter_output = filter_cmd.output().context("running ssh-agent-filter")?;

    if !filter_output.status.success() {
        bail!(
            "ssh-agent-filter failed: {}",
            String::from_utf8_lossy(&filter_output.stderr).trim()
        );
    }

    // Parse SSH_AUTH_SOCK and SSH_AGENT_PID from output
    // Output format: SSH_AUTH_SOCK='...'; export SSH_AUTH_SOCK;\nSSH_AGENT_PID='...'; ...
    let stdout = String::from_utf8_lossy(&filter_output.stdout);

    let filter_sock = stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("SSH_AUTH_SOCK=")
                .map(|rest| {
                    rest.split(';').next().unwrap_or(rest)
                        .trim_matches('\'')
                        .to_string()
                })
        })
        .context("could not parse SSH_AUTH_SOCK from ssh-agent-filter output")?;

    let filter_pid: u32 = stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("SSH_AGENT_PID=")
                .and_then(|rest| {
                    rest.split(';').next().unwrap_or(rest)
                        .trim_matches('\'')
                        .parse().ok()
                })
        })
        .context("could not parse SSH_AGENT_PID from ssh-agent-filter output")?;

    Ok(FilteredAgent {
        sock: PathBuf::from(filter_sock),
        signing_key,
        filter_pid,
        filter_dir,
    })
}

fn cleanup(filter_pid: Option<u32>, filter_dir: Option<&std::path::Path>) {
    if let Some(pid) = filter_pid {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    if let Some(dir) = filter_dir {
        let _ = fs::remove_dir_all(dir);
    }
}
