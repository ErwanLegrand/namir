/// Content identity for a model or IR file (P7, D-11.3): BLAKE3 of the file's raw bytes.
/// An identity, not a security primitive — chosen because it's fast enough to hash a whole
/// library during scanning and isn't a legacy hash Namir will later need to migrate away from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Hashes `bytes` (the whole file's raw contents) into its content identity.
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// The raw 32-byte BLAKE3 digest, for callers that need it outside the hex `Display` form.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_bytes_same_hash() {
        assert_eq!(ContentHash::of(b"hello"), ContentHash::of(b"hello"));
    }

    #[test]
    fn different_bytes_different_hash() {
        assert_ne!(ContentHash::of(b"hello"), ContentHash::of(b"world"));
    }

    #[test]
    fn displays_as_64_hex_chars() {
        let s = ContentHash::of(b"namir").to_string();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn empty_input_is_defined() {
        // Must not panic — an empty .nam file is malformed input the parser will reject
        // elsewhere, but hashing it must still work so it can be reported by hash.
        let _ = ContentHash::of(b"");
    }
}
