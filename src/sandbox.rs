use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use tracing::warn;

use crate::resolve::ResolvedConfig;
use crate::sockets::{self, SocketMount};
use crate::SshAgentInfo;

/// Normalize a path (resolve `.` and `..` components) without following symlinks.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = Vec::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out.iter().collect()
}

/// Check whether `path` or any of its ancestor components is a symlink.
fn contains_symlink(path: &Path) -> bool {
    let mut check = PathBuf::new();
    for c in path.components() {
        check.push(c);
        match check.symlink_metadata() {
            Ok(meta) if meta.is_symlink() => return true,
            _ => {}
        }
    }
    false
}

/// Build the bwrap Command from resolved config.
///
/// Returns the command and an optional `OwnedFd` for the seccomp filter.
/// The caller **must** keep the fd alive until the child process is spawned,
/// because bwrap reads it via `--seccomp FD`.
pub fn build_command(
    config: &ResolvedConfig,
    ssh: Option<&SshAgentInfo>,
) -> (Command, Option<OwnedFd>) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/user".into());
    let home_path = PathBuf::from(&home);

    let mut cmd = Command::new(crate::BWRAP);

    // 1. Read-only base layer
    cmd.arg("--ro-bind").arg("/").arg("/");

    // 2. Fresh devtmpfs
    cmd.arg("--dev").arg("/dev");

    // 3. Fresh procfs
    cmd.arg("--proc").arg("/proc");

    // 4. Clean tmpdir
    cmd.arg("--tmpfs").arg("/tmp");

    // 5. Namespace isolation
    cmd.arg("--unshare-pid");
    cmd.arg("--unshare-ipc");

    // 6. Write path bind-mounts
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
        if file.exists() {
            cmd.arg("--bind").arg(file).arg(file);
        }
    }

    // Configured write paths — reject symlinks to prevent escape
    for path in &config.write_paths {
        let normalized = normalize_path(path);
        if contains_symlink(&normalized) {
            warn!(
                "refusing to mount write path containing symlink: {}",
                path.display()
            );
            continue;
        }
        if normalized.exists() {
            cmd.arg("--bind").arg(&normalized).arg(&normalized);
        }
    }

    // 6b. Protect wrap.toml files — ro-bind after writable mounts to override
    for f in &config.config_files {
        if f.exists() {
            cmd.arg("--ro-bind").arg(f).arg(f);
        }
    }

    // 7. Socket bind-mounts
    let socket_mounts = sockets::resolve_socket_mounts(config);
    for SocketMount {
        host_path,
        sandbox_path,
    } in &socket_mounts
    {
        cmd.arg("--bind").arg(host_path).arg(sandbox_path);
    }

    // 8. SSH agent socket — needs a writable bind-mount because
    //    connect() on a Unix socket requires write permission, and the
    //    ro-bind root makes everything read-only.
    if let Some(ssh) = ssh {
        if let Some(parent) = ssh.sock.parent() {
            cmd.arg("--bind").arg(parent).arg(parent);
        }
    }

    // 9. Environment
    let path_val = std::env::var("PATH").unwrap_or_default();
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

    if let Some(ssh) = ssh {
        cmd.arg("--setenv")
            .arg("SSH_AUTH_SOCK")
            .arg(ssh.sock.to_string_lossy().as_ref());

        // Override git signing config to use the key from the host agent
        let key_literal = format!("key::{}", ssh.signing_key);
        cmd.arg("--setenv").arg("GIT_CONFIG_COUNT").arg("2");
        cmd.arg("--setenv").arg("GIT_CONFIG_KEY_0").arg("user.signingkey");
        cmd.arg("--setenv").arg("GIT_CONFIG_VALUE_0").arg(&key_literal);
        cmd.arg("--setenv").arg("GIT_CONFIG_KEY_1").arg("gpg.ssh.allowedSignersFile");
        cmd.arg("--setenv").arg("GIT_CONFIG_VALUE_1").arg("");
    }

    // 10. Seccomp filter
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    let seccomp_fd = match crate::seccomp::create_filter() {
        Ok(fd) => {
            use std::os::fd::AsRawFd;
            cmd.arg("--seccomp").arg(fd.as_raw_fd().to_string());
            Some(fd)
        }
        Err(e) => {
            warn!("failed to create seccomp filter, proceeding without: {e}");
            None
        }
    };
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let seccomp_fd: Option<OwnedFd> = {
        warn!("seccomp filter not supported on this architecture");
        None
    };

    // 11. Working directory
    cmd.arg("--chdir").arg(&config.cwd);

    // 12. Die with parent
    cmd.arg("--die-with-parent");

    // 13. Command + args
    cmd.arg("--").arg(&config.command);
    // The sandbox is the security boundary — skip Claude's own permission prompts
    if config.command == "claude" {
        cmd.arg("--dangerously-skip-permissions");
    }
    for arg in &config.cmd_args {
        cmd.arg(arg);
    }

    (cmd, seccomp_fd)
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
