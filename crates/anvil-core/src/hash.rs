//! SHA-256 helpers.

use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{IoContext, Result};

/// A [`Read`] adapter that hashes every byte flowing through it.
///
/// Placed directly above the raw file reader, it lets the extractor verify the
/// archive checksum in the very same sequential pass used for decompression,
/// so the (slow) source drive is read exactly once.
pub struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    bytes: u64,
}

impl<R> HashingReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    /// Bytes read so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes
    }

    /// Lower-case hex digest and total byte count.
    pub fn finalize(self) -> (String, u64) {
        (hex::encode(self.hasher.finalize()), self.bytes)
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

/// SHA-256 of a whole file (lower-case hex).
pub fn sha256_file(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).at(path)?;
    let mut reader = HashingReader::new(io::BufReader::with_capacity(1 << 20, file));
    io::copy(&mut reader, &mut io::sink()).at(path)?;
    Ok(reader.finalize().0)
}

/// SHA-256 of a byte slice (lower-case hex).
pub fn sha256_bytes(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashing_reader_matches_one_shot_digest() {
        let data = b"hello anvil".repeat(1000);
        let mut r = HashingReader::new(&data[..]);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        let (digest, n) = r.finalize();
        assert_eq!(out, data);
        assert_eq!(n, data.len() as u64);
        assert_eq!(digest, sha256_bytes(&data));
    }

    #[test]
    fn known_vector() {
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(is_sha256_hex(&sha256_bytes(b"")));
        assert!(!is_sha256_hex("abc"));
    }
}
