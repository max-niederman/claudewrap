/// SSH agent protocol message types.
/// See https://datatracker.ietf.org/doc/html/draft-miller-ssh-agent

// Message type constants — all defined for documentation/completeness.
pub const REQUEST_IDENTITIES: u8 = 11;
pub const SIGN_REQUEST: u8 = 13;
pub const ADD_IDENTITY: u8 = 17;
pub const REMOVE_IDENTITY: u8 = 18;
pub const REMOVE_ALL_IDENTITIES: u8 = 19;
pub const LOCK: u8 = 22;
pub const UNLOCK: u8 = 23;
pub const EXTENSION: u8 = 27;
pub const AGENT_FAILURE: u8 = 5;

/// Read a length-prefixed SSH agent message from a reader.
/// Returns the full message bytes (without the length prefix).
pub fn read_message(reader: &mut impl std::io::Read) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > 256 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid agent message length: {len}"),
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

/// Write a length-prefixed SSH agent message.
pub fn write_message(writer: &mut impl std::io::Write, data: &[u8]) -> std::io::Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    writer.write_all(&len)?;
    writer.write_all(data)?;
    writer.flush()
}

/// Build an SSH_AGENT_FAILURE response.
pub fn failure_response() -> Vec<u8> {
    vec![AGENT_FAILURE]
}

/// Check if a message type should be allowed through the proxy.
pub fn is_allowed(msg_type: u8) -> bool {
    matches!(msg_type, REQUEST_IDENTITIES | SIGN_REQUEST)
}
