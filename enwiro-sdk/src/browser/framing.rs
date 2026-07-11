//! The native messaging wire format: each message is a 4-byte little-endian
//! length prefix followed by that many bytes of JSON, in both directions.

use std::io::{Read, Write};

use anyhow::Context;

/// Upper bound on a single native message. Chrome caps extension-to-host
/// messages at 4 GB but nothing legitimate comes close; a corrupt length
/// prefix must not make us allocate gigabytes.
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Read one length-prefixed message. `Ok(None)` means the peer closed the
/// stream cleanly (EOF on the length prefix), i.e. the browser shut the
/// port down and the host should exit.
pub fn read_message(reader: &mut impl Read) -> anyhow::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    if let Err(e) = reader.read_exact(&mut length) {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(e).context("Could not read native message length");
    }
    let length = u32::from_le_bytes(length) as usize;
    anyhow::ensure!(
        length <= MAX_MESSAGE_BYTES,
        "native message of {} bytes exceeds the {} byte cap",
        length,
        MAX_MESSAGE_BYTES,
    );
    let mut payload = vec![0u8; length];
    reader
        .read_exact(&mut payload)
        .context("Could not read native message payload")?;
    Ok(Some(payload))
}

/// Write one length-prefixed message and flush it.
pub fn write_message(writer: &mut impl Write, payload: &[u8]) -> anyhow::Result<()> {
    let length = u32::try_from(payload.len()).context("Native message too large")?;
    writer
        .write_all(&length.to_le_bytes())
        .and_then(|_| writer.write_all(payload))
        .and_then(|_| writer.flush())
        .context("Could not write native message")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_round_trips() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, br#"{"type":"getRules"}"#).unwrap();
        let mut reader = buffer.as_slice();
        let payload = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(payload, br#"{"type":"getRules"}"#);
        assert!(read_message(&mut reader).unwrap().is_none(), "clean EOF");
    }

    #[test]
    fn read_message_rejects_oversized_length_prefix() {
        let length_prefix = u32::MAX.to_le_bytes();
        let mut reader = length_prefix.as_slice();
        assert!(read_message(&mut reader).is_err());
    }
}
