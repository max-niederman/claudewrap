use std::path::{Path, PathBuf};
use std::process::Command;

use crate::resolve::ResolvedConfig;
use crate::sockets::{self, SocketMount};

/// Build the bwrap Command from resolved config.
pub fn build_command(
    config: &ResolvedConfig,
    proxy_sock: Option<&Path>,
    wrapper_bin_dir: Option<&Path>,
) -> Command {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/user".into());
    let home_path = PathBuf::from(&home);

    let mut cmd = Command::new("bwrap");

    // 1. Read-only base layer
    cmd.arg("--ro-bind").arg("/").arg("/");

    // 2. Fresh devtmpfs
    cmd.arg("--dev").arg("/dev");

    // 3. Fresh procfs
    cmd.arg("--proc").arg("/proc");

    // 4. Clean tmpdir
    cmd.arg("--tmpfs").arg("/tmp");

    // 5. Write path bind-mounts
    // Implicit always-writable paths
    let implicit_writes = [
        home_path.join(".claude"),
        home_path.join(".claude.json"),
        config.cwd.join(".claude"),
    ];

    for path in &implicit_writes {
        if path.exists() {
            cmd.arg("--bind").arg(path).arg(path);
        }
    }

    // Configured write paths
    for path in &config.write_paths {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        cmd.arg("--bind").arg(&canonical).arg(&canonical);
    }

    // 6. Socket bind-mounts
    let socket_mounts = sockets::resolve_socket_mounts(config);
    for SocketMount {
        host_path,
        sandbox_path,
    } in &socket_mounts
    {
        cmd.arg("--bind").arg(host_path).arg(sandbox_path);
    }

    // 7. SSH agent proxy socket — needs a writable bind-mount because
    //    connect() on a Unix socket requires write permission, and the
    //    ro-bind root makes everything read-only.
    if let Some(sock) = proxy_sock {
        // Bind-mount the parent directory so the socket is connectable
        if let Some(parent) = sock.parent() {
            cmd.arg("--bind").arg(parent).arg(parent);
        }
    }

    // 8. Environment
    let mut path_val = std::env::var("PATH").unwrap_or_default();
    if let Some(bin_dir) = wrapper_bin_dir {
        // Prepend wrapper dir to shadow real ssh
        path_val = format!("{}:{path_val}", bin_dir.display());
    }

    cmd.arg("--setenv").arg("PATH").arg(&path_val);

    for var in &[
        "HOME",
        "USER",
        "TERM",
        "LANG",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DISPLAY",
    ] {
        if let Ok(val) = std::env::var(var) {
            cmd.arg("--setenv").arg(var).arg(&val);
        }
    }

    if let Some(sock) = proxy_sock {
        cmd.arg("--setenv")
            .arg("SSH_AUTH_SOCK")
            .arg(sock.to_string_lossy().as_ref());
    } else if let Ok(val) = std::env::var("SSH_AUTH_SOCK") {
        cmd.arg("--setenv").arg("SSH_AUTH_SOCK").arg(&val);
    }

    // 9. Working directory
    cmd.arg("--chdir").arg(&config.cwd);

    // 10. Die with parent
    cmd.arg("--die-with-parent");

    // 11. New session
    cmd.arg("--new-session");

    // 12. Command + args
    cmd.arg("--").arg(&config.command);
    for arg in &config.cmd_args {
        cmd.arg(arg);
    }

    cmd
}

/// Format the command for --dry-run display.
pub fn format_command(cmd: &Command) -> String {
    let mut parts = vec![cmd.get_program().to_string_lossy().into_owned()];
    for arg in cmd.get_args() {
        let s = arg.to_string_lossy();
        if s.contains(' ') || s.contains('\'') || s.contains('"') || s.is_empty() {
            parts.push(format!("'{}'", s.replace('\'', "'\\''")));
        } else {
            parts.push(s.into_owned());
        }
    }
    parts.join(" \\\n  ")
}
