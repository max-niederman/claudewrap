mod cli;
mod config;
mod resolve;
mod sandbox;
mod sockets;

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
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
        info!(ssh = config.ssh.allow_ssh, hosts = ?config.ssh.allow_hosts, "ssh");
        info!(command = config.command, args = ?config.cmd_args, "command");
    }

    // Ensure sandbox SSH key exists
    let ssh_key = ensure_sandbox_key()?;

    // Create per-PID temp dir (for SSH wrapper script)
    let pid = std::process::id();
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| "/tmp".into());
    let temp_dir = PathBuf::from(&runtime_dir).join(format!("claudewrap-{pid}"));
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("creating temp dir {}", temp_dir.display()))?;

    // Start dedicated ssh-agent with only the sandbox key
    let agent_dir = PathBuf::from(&runtime_dir).join("claudewrap");
    fs::create_dir_all(&agent_dir)
        .with_context(|| format!("creating agent dir {}", agent_dir.display()))?;
    let agent_sock = agent_dir.join("agent.sock");

    // Remove stale socket if it exists
    if agent_sock.exists() {
        let _ = fs::remove_file(&agent_sock);
    }

    let agent_pid = start_ssh_agent(&agent_sock, &ssh_key)?;
    info!("ssh-agent started (pid {agent_pid}) at {}", agent_sock.display());

    // Generate SSH wrapper if SSH is restricted
    let wrapper_bin_dir = if !config.ssh.allow_ssh && !config.ssh_allow_all {
        Some(generate_ssh_wrapper(&temp_dir, &config)?)
    } else {
        None
    };

    // Build bwrap command
    let cmd = sandbox::build_command(
        &config,
        Some(&agent_sock),
        wrapper_bin_dir.as_deref(),
    );

    if config.dry_run {
        println!("{}", sandbox::format_command(&cmd));
        cleanup(&temp_dir, Some(agent_pid));
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

    cleanup(&temp_dir, Some(agent_pid));

    Ok(ExitCode::from(
        status.code().unwrap_or(1) as u8,
    ))
}

/// Ensure ~/.ssh/id_claudewrap_ed25519 exists, prompting to create if missing.
fn ensure_sandbox_key() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let key_path = PathBuf::from(&home).join(".ssh/id_claudewrap_ed25519");

    if key_path.exists() {
        return Ok(key_path);
    }

    eprint!("No sandbox SSH key found. Create {}? [y/N] ", key_path.display());
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;

    if answer.trim().eq_ignore_ascii_case("y") {
        // Ensure ~/.ssh exists
        if let Some(ssh_dir) = key_path.parent() {
            fs::create_dir_all(ssh_dir)?;
        }

        let status = std::process::Command::new("ssh-keygen")
            .args([
                "-t", "ed25519",
                "-f", &key_path.to_string_lossy(),
                "-N", "",
                "-C", "claudewrap sandbox key",
            ])
            .status()
            .context("running ssh-keygen")?;

        if !status.success() {
            bail!("ssh-keygen failed");
        }

        let pub_path = key_path.with_file_name("id_claudewrap_ed25519.pub");
        eprintln!("Key created. Add the public key to GitHub:");
        eprintln!("  {}", pub_path.display());
        Ok(key_path)
    } else {
        bail!(
            "sandbox SSH key is required; create it with:\n  \
             ssh-keygen -t ed25519 -f {} -N \"\" -C \"claudewrap sandbox key\"",
            key_path.display()
        );
    }
}

/// Spawn ssh-agent bound to `sock_path`, add `key_path`, return agent PID.
fn start_ssh_agent(sock_path: &Path, key_path: &Path) -> Result<u32> {
    // Start ssh-agent with a dedicated socket
    let output = std::process::Command::new("ssh-agent")
        .arg("-a")
        .arg(sock_path)
        .output()
        .context("spawning ssh-agent")?;

    if !output.status.success() {
        bail!(
            "ssh-agent failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Parse PID from ssh-agent output (e.g. "SSH_AGENT_PID=12345; ...")
    let stdout = String::from_utf8_lossy(&output.stdout);
    let agent_pid: u32 = stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("SSH_AGENT_PID=")
                .and_then(|rest| rest.trim_end_matches(';').trim().parse().ok())
        })
        .or_else(|| {
            // Also try "echo Agent pid NNN;" format
            stdout.lines().find_map(|line| {
                line.strip_prefix("echo Agent pid ")
                    .and_then(|rest| rest.trim_end_matches(';').trim().parse().ok())
            })
        })
        .context("could not parse SSH_AGENT_PID from ssh-agent output")?;

    // Add the sandbox key
    let add_status = std::process::Command::new("ssh-add")
        .arg(key_path)
        .env("SSH_AUTH_SOCK", sock_path)
        .status()
        .context("running ssh-add")?;

    if !add_status.success() {
        // Kill the agent we just started before bailing
        unsafe { libc::kill(agent_pid as i32, libc::SIGTERM); }
        bail!("ssh-add failed to load sandbox key");
    }

    Ok(agent_pid)
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
exec {real_ssh} -F /dev/null "${{args[@]}}"
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

fn cleanup(temp_dir: &Path, agent_pid: Option<u32>) {
    let _ = fs::remove_dir_all(temp_dir);
    if let Some(pid) = agent_pid {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
}
