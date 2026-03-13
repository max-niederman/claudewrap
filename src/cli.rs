use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "claudewrap",
    about = "Sandbox Claude Code (or other commands) with bubblewrap",
    after_help = "Everything after `--` is passed to the inner command (claude by default)."
)]
pub struct Cli {
    /// Select scope by ID (from .claude/wrap.toml ancestry)
    #[arg(short, long, value_name = "ID")]
    pub scope: Option<String>,

    /// Grant ad-hoc write access (repeatable)
    #[arg(short, long, value_name = "PATH")]
    pub write: Vec<PathBuf>,

    /// Run CMD instead of `claude` inside sandbox
    #[arg(short, long, value_name = "CMD")]
    pub exec: Option<String>,

    /// Force-enable Wayland socket
    #[arg(long, conflicts_with = "no_wayland")]
    pub wayland: bool,

    /// Force-disable Wayland socket
    #[arg(long)]
    pub no_wayland: bool,

    /// Force-enable PipeWire socket
    #[arg(long)]
    pub pipewire: bool,

    /// Enable D-Bus socket ("session" or "system")
    #[arg(long, value_name = "MODE")]
    pub dbus: Option<String>,

    /// Ad-hoc SSH host allowlist (repeatable)
    #[arg(long, value_name = "HOST")]
    pub ssh_allow_hosts: Vec<String>,

    /// Unrestricted SSH access
    #[arg(long)]
    pub ssh_allow_all: bool,

    /// Print bwrap command, don't execute
    #[arg(long)]
    pub dry_run: bool,

    /// Show resolved config
    #[arg(long)]
    pub verbose: bool,

    /// Arguments passed to the inner command
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub cmd_args: Vec<String>,
}
