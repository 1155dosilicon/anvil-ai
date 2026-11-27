//! Conversion between local filesystem paths and `file://` URIs (Windows + Unix).

use std::path::{Path, PathBuf};

use crate::error::{LspError, Result};

/// Convert an absolute (or relative, made absolute) path into a `file://` URI.
pub fn path_to_uri(path: &Path) -> Result<String> {
    let raw_in = path.to_string_lossy();
    let looks_like_win_abs = raw_in.starts_with(r"\\?\")
        || (raw_in.len() >= 2 && raw_in.as_bytes()[0].is_ascii_alphabetic() && raw_in.as_bytes()[1] == b':');
    let abs = if path.is_absolute() || looks_like_win_abs {
        path.to_path_buf()
    } else {
        std::path::absolute(path).map_err(|e| LspError::InvalidPath(format!("{}: {e}", path.display())))?
    };

    // Strip Windows extended-length prefix (`\\?\C:\...` -> `C:\...`).
    let raw = abs.to_string_lossy();
    let stripped = raw
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| raw.strip_prefix(r"\\?\").map(str::to_string))
        .unwrap_or_else(|| raw.into_owned());

    // Windows drive letter path (e.g. `C:\foo\bar` -> `file:///C:/foo/bar`).
    let bytes = stripped.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_uppercase();
        let rest = stripped[2..].replace('\\', "/");
        let encoded = percent_encode_path(&rest);
        return Ok(format!("file:///{drive}:{encoded}"));
    }

    // Unix absolute path (`/foo/bar` -> `file:///foo/bar`).
    let unix = stripped.replace('\\', "/");
    if unix.starts_with('/') {
        let encoded = percent_encode_path(&unix);
        return Ok(format!("file://{encoded}"));
    }

    Err(LspError::InvalidPath(format!(
        "cannot convert '{}' to file:// URI",
        abs.display()
    )))
}

/// Convert a `file://` URI back into a local [`PathBuf`].
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path_part = if let Some(after_localhost) = rest.strip_prefix("localhost/") {
        format!("/{after_localhost}")
    } else if rest.starts_with('/') {
        rest.to_string()
    } else {
        return None;
    };

    let decoded = percent_decode(&path_part)?;

    // Windows drive URI: `/C:/foo/bar` or `/c:/foo/bar`.
    let bytes = decoded.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        let drive = (bytes[1] as char).to_ascii_uppercase();
        let rest = &decoded[2..];
        if cfg!(windows) {
            return Some(PathBuf::from(format!("{drive}{}", rest.replace('/', "\\"))));
        }
        return Some(PathBuf::from(format!("{drive}{rest}")));
    }

    Some(PathBuf::from(decoded))
}

/// Normalise a `file://` URI for map lookup (case-insensitive drive letter on Windows).
pub fn normalize_uri(uri: &str) -> String {
    if let Some(path) = uri_to_path(uri) {
        if cfg!(windows) {
            return path.to_string_lossy().replace('/', "\\").to_lowercase();
        }
        return path.to_string_lossy().into_owned();
    }
    uri.to_string()
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        let safe = byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~');
        if safe {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = from_hex(bytes[i + 1])?;
            let lo = from_hex(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_drive_uri_round_trip() {
        let uri = path_to_uri(Path::new(r"C:\Users\Eli\my project\main.rs")).unwrap();
        assert_eq!(uri, "file:///C:/Users/Eli/my%20project/main.rs");
        let back = uri_to_path(&uri).unwrap();
        if cfg!(windows) {
            assert_eq!(back, PathBuf::from(r"C:\Users\Eli\my project\main.rs"));
        } else {
            assert_eq!(back, PathBuf::from("C:/Users/Eli/my project/main.rs"));
        }
    }

    #[test]
    fn unix_uri_round_trip() {
        let back = uri_to_path("file:///home/eli/my%20proj/src/lib.rs").unwrap();
        assert_eq!(back, PathBuf::from("/home/eli/my proj/src/lib.rs"));
    }

    #[test]
    fn percent_encoded_colon_decodes_cleanly() {
        let back = uri_to_path("file:///c%3A/dev/hello.c").unwrap();
        if cfg!(windows) {
            assert_eq!(back, PathBuf::from(r"C:\dev\hello.c"));
        } else {
            assert_eq!(back, PathBuf::from("C:/dev/hello.c"));
        }
    }

    #[test]
    fn windows_lowercase_drive_uri_normalizes_to_uppercase() {
        let back = uri_to_path("file:///c:/Users/Eli/rtest/src/main.rs").unwrap();
        if cfg!(windows) {
            assert_eq!(back, PathBuf::from(r"C:\Users\Eli\rtest\src\main.rs"));
        } else {
            assert_eq!(back, PathBuf::from("C:/Users/Eli/rtest/src/main.rs"));
        }
        let d_back = uri_to_path("file:///d:/anvil/src/lib.rs").unwrap();
        if cfg!(windows) {
            assert_eq!(d_back, PathBuf::from(r"D:\anvil\src\lib.rs"));
        } else {
            assert_eq!(d_back, PathBuf::from("D:/anvil/src/lib.rs"));
        }
    }
}
