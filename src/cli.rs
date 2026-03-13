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
