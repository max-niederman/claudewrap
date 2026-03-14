//! SSH agent filtering proxy.
//!
//! Listens on a Unix socket and forwards only allowed operations to an
//! upstream SSH agent. Only keys whose blobs appear in `allowed_keys`
//! are visible or signable. All other message types are rejected.

use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Maximum message size (256 KiB, same as OpenSSH).
const MAX_MSG_LEN: u32 = 256 * 1024;

/// Maximum concurrent client connections.
const MAX_CONNECTIONS: usize = 16;

// Message types
const SSH_AGENT_FAILURE: u8 = 5;
const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;
const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;
const SSH_AGENTC_SIGN_REQUEST: u8 = 13;
const SSH_AGENT_SIGN_RESPONSE: u8 = 14;

/// A raw SSH agent message (type byte + payload, without length prefix).
struct AgentMsg {
    msg_type: u8,
    payload: Vec<u8>,
}

fn read_msg(stream: &mut impl Read) -> io::Result<AgentMsg> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);

    if len == 0 || len > MAX_MSG_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid message length: {len}"),
        ));
    }

    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf)?;

    let msg_type = buf[0];
    let payload = buf[1..].to_vec();
    Ok(AgentMsg { msg_type, payload })
}

fn write_msg(stream: &mut impl Write, msg_type: u8, payload: &[u8]) -> io::Result<()> {
    let len = (1 + payload.len()) as u32;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(&[msg_type])?;
    stream.write_all(payload)?;
    stream.flush()
}

fn write_failure(stream: &mut impl Write) -> io::Result<()> {
    write_msg(stream, SSH_AGENT_FAILURE, &[])
}

/// Read a `string` (uint32 length + data) from `buf` starting at `offset`.
/// Returns the string bytes and the new offset, or None if out of bounds.
fn read_string(buf: &[u8], offset: usize) -> Option<(&[u8], usize)> {
    if offset + 4 > buf.len() {
        return None;
    }
    let len = u32::from_be_bytes(buf[offset..offset + 4].try_into().ok()?) as usize;
    let end = offset + 4 + len;
    if end > buf.len() {
        return None;
    }
    Some((&buf[offset + 4..end], end))
}

/// Write a `string` (uint32 length + data) into `out`.
fn write_string(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
}

/// Filter an IDENTITIES_ANSWER to only include allowed keys.
fn filter_identities(payload: &[u8], allowed: &[Vec<u8>]) -> Option<Vec<u8>> {
    if payload.len() < 4 {
        return None;
    }

    let nkeys = u32::from_be_bytes(payload[0..4].try_into().ok()?) as usize;
    let mut offset = 4;
    let mut filtered_keys: Vec<(&[u8], &[u8])> = Vec::new();

    for _ in 0..nkeys {
        let (key_blob, next) = read_string(payload, offset)?;
        let (comment, next) = read_string(payload, next)?;

        if allowed.iter().any(|k| k.as_slice() == key_blob) {
            filtered_keys.push((key_blob, comment));
        }

        offset = next;
    }

    let mut out = Vec::new();
    out.extend_from_slice(&(filtered_keys.len() as u32).to_be_bytes());
    for (key_blob, comment) in &filtered_keys {
        write_string(&mut out, key_blob);
        write_string(&mut out, comment);
    }
    Some(out)
}

/// Extract the key blob from a SIGN_REQUEST payload.
fn sign_request_key_blob(payload: &[u8]) -> Option<&[u8]> {
    let (key_blob, _) = read_string(payload, 0)?;
    Some(key_blob)
}

/// Handle a single client connection.
fn handle_client(
    mut client: UnixStream,
    upstream_path: &Path,
    allowed: &[Vec<u8>],
) {
    let mut handle = || -> io::Result<()> {
        loop {
            let msg = match read_msg(&mut client) {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            };

            match msg.msg_type {
                SSH_AGENTC_REQUEST_IDENTITIES => {
                    // Forward to upstream, filter the response
                    let mut upstream = UnixStream::connect(upstream_path)?;
                    write_msg(&mut upstream, msg.msg_type, &msg.payload)?;
                    let resp = read_msg(&mut upstream)?;

                    if resp.msg_type == SSH_AGENT_IDENTITIES_ANSWER {
                        if let Some(filtered) = filter_identities(&resp.payload, allowed) {
                            write_msg(&mut client, SSH_AGENT_IDENTITIES_ANSWER, &filtered)?;
                        } else {
                            write_failure(&mut client)?;
                        }
                    } else {
                        // Unexpected response type — don't forward, just fail
                        write_failure(&mut client)?;
                    }
                }

                SSH_AGENTC_SIGN_REQUEST => {
                    // Check the key blob is in our allowed list
                    let allowed_key = sign_request_key_blob(&msg.payload)
                        .map(|blob| allowed.iter().any(|k| k.as_slice() == blob))
                        .unwrap_or(false);

                    if allowed_key {
                        let mut upstream = UnixStream::connect(upstream_path)?;
                        write_msg(&mut upstream, msg.msg_type, &msg.payload)?;
                        let resp = read_msg(&mut upstream)?;
                        // Only forward expected response types
                        if resp.msg_type == SSH_AGENT_SIGN_RESPONSE
                            || resp.msg_type == SSH_AGENT_FAILURE
                        {
                            write_msg(&mut client, resp.msg_type, &resp.payload)?;
                        } else {
                            write_failure(&mut client)?;
                        }
                    } else {
                        write_failure(&mut client)?;
                    }
                }

                // Reject everything else: add, remove, lock, unlock, extensions, etc.
                _ => {
                    write_failure(&mut client)?;
                }
            }
        }
    };

    if let Err(e) = handle() {
        // Connection errors are normal (client disconnect, etc.)
        let _ = e;
    }
}

/// Start the filtering proxy. Returns the socket path.
/// The proxy runs in a background thread and accepts connections until
/// the listener is dropped (socket file removed).
pub fn start(
    upstream_sock: &Path,
    allowed_key_blobs: Vec<Vec<u8>>,
) -> io::Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let proxy_dir = PathBuf::from(&runtime_dir).join("claudewrap");
    std::fs::create_dir_all(&proxy_dir)?;

    let sock_path = proxy_dir.join(format!("agent.{}", std::process::id()));

    // Remove stale socket
    let _ = std::fs::remove_file(&sock_path);

    let listener = UnixListener::bind(&sock_path)?;

    // Restrict socket permissions to owner only
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))?;
    let upstream = upstream_sock.to_path_buf();
    let allowed = Arc::new(allowed_key_blobs);

    let active = Arc::new(AtomicUsize::new(0));

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(client) => {
                    if active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                        // Drop the connection silently
                        drop(client);
                        continue;
                    }
                    let upstream = upstream.clone();
                    let allowed = Arc::clone(&allowed);
                    let active = Arc::clone(&active);
                    active.fetch_add(1, Ordering::Relaxed);
                    std::thread::spawn(move || {
                        handle_client(client, &upstream, &allowed);
                        active.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(_) => break,
            }
        }
    });

    Ok(sock_path)
}
