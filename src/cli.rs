use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "claudewrap",
    about = "Sandbox Claude Code (or other commands) with bubblewrap",
    after_help = "Everything after `--` is passed to the inner command (claude by default)."
)]
pub struct Cli {
    /// Add a scope by ID (repeatable; adds to default scopes)
    #[arg(short, long, value_name = "ID")]
    pub scope: Vec<String>,

    /// Grant ad-hoc write access (repeatable)
    #[arg(short, long, value_name = "PATH")]
    pub write: Vec<PathBuf>,

    /// Grant ad-hoc read-only access (repeatable). Cancels a default mask
    /// when the path matches exactly.
    #[arg(short, long, value_name = "PATH")]
    pub read: Vec<PathBuf>,

    /// Hide a path from the sandbox (repeatable). Directories become tmpfs,
    /// files are shadowed with /dev/null.
    #[arg(long, value_name = "PATH")]
    pub mask: Vec<PathBuf>,

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

    /// Enable Docker socket passthrough
    #[arg(long)]
    pub docker: bool,

    /// Enable D-Bus socket ("session" or "system")
    #[arg(long, value_name = "MODE")]
    pub dbus: Option<String>,

    /// SSH key fingerprint to allow from host agent (repeatable, e.g. "SHA256:...")
    #[arg(long, value_name = "FINGERPRINT")]
    pub ssh_key: Vec<String>,

    /// Disable SSH agent passthrough
    #[arg(long)]
    pub no_ssh_agent: bool,

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
