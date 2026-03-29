use std::path::PathBuf;

use crate::config::DbusMode;
use crate::resolve::ResolvedConfig;

/// A socket that should be bind-mounted into the sandbox.
#[derive(Debug)]
pub struct SocketMount {
    pub host_path: PathBuf,
    pub sandbox_path: PathBuf,
}

/// Resolve which sockets should be mounted based on config.
pub fn resolve_socket_mounts(config: &ResolvedConfig) -> Vec<SocketMount> {
    let mut mounts = Vec::new();
    let uid = unsafe { libc::getuid() };
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{uid}"));

    if config.wayland {
        let display =
            std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-1".into());
        let path = PathBuf::from(&runtime_dir).join(&display);
        if path.exists() {
            mounts.push(SocketMount {
                sandbox_path: path.clone(),
                host_path: path,
            });
        }
    }

    if config.pipewire {
        let path = PathBuf::from(&runtime_dir).join("pipewire-0");
        if path.exists() {
            mounts.push(SocketMount {
                sandbox_path: path.clone(),
                host_path: path,
            });
        }
    }

    if config.docker {
        let sock = PathBuf::from("/run/docker.sock");
        if sock.exists() {
            mounts.push(SocketMount {
                sandbox_path: sock.clone(),
                host_path: sock,
            });
        }
    }

    match config.dbus {
        DbusMode::Session => {
            let path = PathBuf::from(&runtime_dir).join("bus");
            if path.exists() {
                mounts.push(SocketMount {
                    sandbox_path: path.clone(),
                    host_path: path,
                });
            }
        }
        DbusMode::System => {
            let path = PathBuf::from("/run/dbus/system_bus_socket");
            if path.exists() {
                mounts.push(SocketMount {
                    sandbox_path: path.clone(),
                    host_path: path,
                });
            }
        }
        DbusMode::Disabled => {}
    }

    mounts
}
