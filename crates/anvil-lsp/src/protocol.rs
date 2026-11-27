//! Low-level LSP `Content-Length` JSON-RPC framing.

use std::io::{BufRead, Write};

use serde_json::Value;

use crate::error::{LspError, Result};

/// Write one LSP JSON-RPC message framed with `Content-Length`.
pub fn write_message<W: Write>(writer: &mut W, msg: &Value) -> Result<()> {
    let payload = serde_json::to_vec(msg)?;
    write!(writer, "Content-Length: {}\r\n\r\n", payload.len())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

/// Read one LSP JSON-RPC message framed with `Content-Length`.
///
/// Returns `Ok(None)` on clean EOF before the next header.
pub fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            if content_length.is_none() {
                return Ok(None);
            }
            return Err(LspError::Disconnected);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if content_length.is_some() {
                break;
            }
            // Tolerate leading blank lines.
            continue;
        }
        if let Some((name, val)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                let len: usize = val
                    .trim()
                    .parse()
                    .map_err(|_| LspError::InvalidPath(format!("invalid Content-Length header: {trimmed}")))?;
                content_length = Some(len);
            }
        }
    }

    let len = content_length.ok_or(LspError::Disconnected)?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let value: Value = serde_json::from_slice(&body)?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn round_trip_framing() {
        let mut buf = Vec::new();
        let m1 = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let m2 = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
        write_message(&mut buf, &m1).unwrap();
        write_message(&mut buf, &m2).unwrap();

        let mut cursor = Cursor::new(buf);
        assert_eq!(read_message(&mut cursor).unwrap(), Some(m1));
        assert_eq!(read_message(&mut cursor).unwrap(), Some(m2));
        assert_eq!(read_message(&mut cursor).unwrap(), None);
    }

    #[test]
    fn invalid_json_body_keeps_framing_in_sync() {
        let mut buf = b"Content-Length: 5\r\n\r\n{oops".to_vec();
        let good = json!({"jsonrpc": "2.0", "method": "x"});
        write_message(&mut buf, &good).unwrap();
        let mut cursor = Cursor::new(buf);
        assert!(matches!(read_message(&mut cursor), Err(LspError::Json(_))));
        assert_eq!(read_message(&mut cursor).unwrap(), Some(good));
    }
}
