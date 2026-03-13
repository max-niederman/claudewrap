use std::path::{Path, PathBuf};
use std::process::Command;

use crate::resolve::ResolvedConfig;
use crate::sockets::{self, SocketMount};

/// Build the bwrap Command from resolved config.
pub fn build_command(
    config: &ResolvedConfig,
    agent_sock: Option<&Path>,
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
    // Implicit always-writable paths — ensure they exist so bwrap can bind them
    let implicit_dirs = [
        home_path.join(".claude"),
        config.cwd.join(".claude"),
    ];
    let implicit_files = [
        home_path.join(".claude.json"),
    ];

    for dir in &implicit_dirs {
        let _ = std::fs::create_dir_all(dir);
        cmd.arg("--bind").arg(dir).arg(dir);
    }
    for file in &implicit_files {
        if !file.exists() {
            let _ = std::fs::File::create(file);
        }
        cmd.arg("--bind").arg(file).arg(file);
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

    // 7. SSH agent socket — needs a writable bind-mount because
    //    connect() on a Unix socket requires write permission, and the
    //    ro-bind root makes everything read-only.
    if let Some(sock) = agent_sock {
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

    if let Some(sock) = agent_sock {
        cmd.arg("--setenv")
            .arg("SSH_AUTH_SOCK")
            .arg(sock.to_string_lossy().as_ref());

        // Override git signing config to use the sandbox key
        let sandbox_pub_key = PathBuf::from(&home).join(".ssh/id_claudewrap_ed25519.pub");
        cmd.arg("--setenv").arg("GIT_CONFIG_COUNT").arg("2");
        cmd.arg("--setenv").arg("GIT_CONFIG_KEY_0").arg("user.signingkey");
        cmd.arg("--setenv").arg("GIT_CONFIG_VALUE_0").arg(&sandbox_pub_key);
        cmd.arg("--setenv").arg("GIT_CONFIG_KEY_1").arg("gpg.ssh.allowedSignersFile");
        cmd.arg("--setenv").arg("GIT_CONFIG_VALUE_1").arg("");
    }

    // 9. Working directory
    cmd.arg("--chdir").arg(&config.cwd);

    // 10. Die with parent
    cmd.arg("--die-with-parent");

    // 11. New session
    cmd.arg("--new-session");

    // 12. Command + args
    cmd.arg("--").arg(&config.command);
    // The sandbox is the security boundary — skip Claude's own permission prompts
    if config.command == "claude" {
        cmd.arg("--dangerously-skip-permissions");
    }
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
