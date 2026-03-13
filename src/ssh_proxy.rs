use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;

use anyhow::{Context, Result};
use tracing::{debug, warn};

use crate::ssh_protocol;

/// Start the SSH agent proxy. Returns the path to the proxy socket.
/// The proxy runs on a background thread and filters SSH agent protocol messages.
pub fn start_proxy(
    proxy_dir: &Path,
    real_agent_sock: &str,
) -> Result<(PathBuf, thread::JoinHandle<()>)> {
    let proxy_sock_path = proxy_dir.join("agent.sock");
    let listener = UnixListener::bind(&proxy_sock_path)
        .with_context(|| format!("binding proxy socket at {}", proxy_sock_path.display()))?;

    let real_sock = real_agent_sock.to_string();

    let handle = thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(client) => {
                    let real = real_sock.clone();
                    thread::spawn(move || {
                        if let Err(e) = handle_client(client, &real) {
                            debug!("proxy client disconnected: {e}");
                        }
                    });
                }
                Err(e) => {
                    debug!("proxy listener ended: {e}");
                    break;
                }
            }
        }
    });

    Ok((proxy_sock_path, handle))
}

fn handle_client(mut client: UnixStream, real_agent_path: &str) -> Result<()> {
    let mut upstream = match UnixStream::connect(real_agent_path) {
        Ok(s) => s,
        Err(e) => {
            warn!("cannot connect to real agent at {real_agent_path}: {e}");
            // Serve failure responses until the client disconnects
            return serve_failures(&mut client);
        }
    };

    loop {
        let msg = match ssh_protocol::read_message(&mut client) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        if msg.is_empty() {
            continue;
        }

        let msg_type = msg[0];

        if ssh_protocol::is_allowed(msg_type) {
            // Forward to real agent, falling back to failure on upstream error
            if let Err(e) = ssh_protocol::write_message(&mut upstream, &msg) {
                warn!("upstream write failed: {e}");
                ssh_protocol::write_message(&mut client, &ssh_protocol::failure_response())?;
                continue;
            }
            match ssh_protocol::read_message(&mut upstream) {
                Ok(response) => ssh_protocol::write_message(&mut client, &response)?,
                Err(e) => {
                    warn!("upstream read failed: {e}");
                    ssh_protocol::write_message(&mut client, &ssh_protocol::failure_response())?;
                }
            }
        } else {
            warn!(msg_type, "blocked SSH agent request");
            ssh_protocol::write_message(&mut client, &ssh_protocol::failure_response())?;
        }
    }
}

/// Send AGENT_FAILURE for every request until the client disconnects.
fn serve_failures(client: &mut UnixStream) -> Result<()> {
    loop {
        match ssh_protocol::read_message(client) {
            Ok(_) => {
                ssh_protocol::write_message(client, &ssh_protocol::failure_response())?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
}
