/// Content identity for a model or IR file (P7, D-11.3): BLAKE3 of the file's raw bytes.
/// An identity, not a security primitive — chosen because it's fast enough to hash a whole
/// library during scanning and isn't a legacy hash Namir will later need to migrate away from.
///
/// # M5's additions, and why they were missing until now
///
/// Before M5, this type could be produced and displayed but never round-tripped: `Display` writes
/// lowercase hex, but there was no `FromStr`/`from_bytes` to read it back, and the `[u8; 32]` field
/// is private with no public constructor at all. That was fine while the only consumer was
/// `namir-worker`'s cache, which always derives a `ContentHash` fresh from bytes it already has in
/// hand. M5's `namir-state` stores a hash as 64 hex characters in a preset document (FR-STATE-040
/// demands human-readable, so hex, not a serde derive over the raw bytes) and `namir-library`
/// stores one per index entry — both need to read a hash back, not just produce one. `FromStr` and
/// `from_bytes` close that gap; `Ord`/`PartialOrd` support a `BTreeMap<ContentHash, _>` for
/// `namir-library`'s hash → path index (D-11.3's consequence note in `02-architecture.md` §11), for
/// the same stable-ordering reason D-11.1 wants sorted JSON keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash([u8; 32]);

/// Why [`ContentHash::from_hex`]/[`std::str::FromStr`] rejected a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentHashParseError {
    /// Not exactly 64 bytes long (BLAKE3's digest is fixed-size; anything else cannot be one).
    WrongLength,
    /// Contains a byte that is not an ASCII hex digit.
    NotHex,
}

impl std::fmt::Display for ContentHashParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongLength => write!(f, "content hash must be exactly 64 hex characters"),
            Self::NotHex => write!(f, "content hash must contain only hex digits"),
        }
    }
}

impl std::error::Error for ContentHashParseError {}

impl ContentHash {
    /// Hashes `bytes` (the whole file's raw contents) into its content identity.
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Builds a `ContentHash` directly from a 32-byte digest already computed elsewhere — the
    /// counterpart to [`Self::as_bytes`], and what makes deserialising one from a stored document
    /// possible at all (previously the field was private with no public constructor of any kind).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Parses 64 lowercase-or-uppercase hex characters (as produced by [`Display`](Self), which is
    /// always lowercase, but a hand-edited preset per FR-STATE-040 may not be) into a `ContentHash`.
    /// The non-panicking counterpart to `FromStr::from_str`, named so a caller reading the code
    /// doesn't have to know `FromStr` is implemented to find it.
    pub fn from_hex(s: &str) -> Result<Self, ContentHashParseError> {
        let s = s.as_bytes();
        if s.len() != 64 {
            return Err(ContentHashParseError::WrongLength);
        }
        let mut out = [0u8; 32];
        // `as_chunks` rather than `chunks_exact`: the length is already known to be 64, so the
        // remainder is provably empty and each pair arrives as a `[u8; 2]` that indexes without a
        // bounds check.
        for (i, &[hi, lo]) in s.as_chunks::<2>().0.iter().enumerate() {
            let hi = hex_nibble(hi).ok_or(ContentHashParseError::NotHex)?;
            let lo = hex_nibble(lo).ok_or(ContentHashParseError::NotHex)?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }

    /// The raw 32-byte BLAKE3 digest, for callers that need it outside the hex `Display` form.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl std::str::FromStr for ContentHash {
    type Err = ContentHashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s)
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

/// Streams bytes into a [`ContentHash`] without holding the whole input in memory at once (D-2.5's
/// namir-library scan benchmark hashes up to [`crate::MAX_FILE_BYTES`] per file, on each of a
/// worker pool's threads concurrently — buffering that much per thread is a large transient this
/// type exists to avoid). A thin wrapper over `blake3::Hasher` so `namir-library` and `namir-state`
/// never need their own `blake3` dependency; `namir-core` is already the one crate that has it.
#[derive(Debug, Default, Clone)]
pub struct ContentHasher(blake3::Hasher);

impl ContentHasher {
    /// A hasher with no input yet — `ContentHash::of(b"")`'s identity if finished immediately.
    pub fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    /// Feeds more bytes in. Callable any number of times, in any chunk size.
    pub fn update(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.update(bytes);
        self
    }

    /// Finalises the hash. Consumes `self` because BLAKE3's finalisation, unlike its update step,
    /// is not incremental — calling this and then updating further would silently discard the
    /// finalisation cost already paid, which a `&self` signature would make easy to do by mistake.
    pub fn finish(self) -> ContentHash {
        ContentHash(*self.0.finalize().as_bytes())
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

    /// FR-STATE-040 / D-11.3: a hash stored as hex in a preset document must read back to the
    /// same `ContentHash` that produced it — `Display` alone (write-only until M5) cannot satisfy
    /// that, which is why `namir-state` and `namir-library` both need this direction to exist.
    #[test]
    fn hex_round_trips_through_display_and_from_hex() {
        let hash = ContentHash::of(b"namir round trip");
        let hex = hash.to_string();
        assert_eq!(ContentHash::from_hex(&hex), Ok(hash));
        assert_eq!(hex.parse::<ContentHash>(), Ok(hash));
    }

    #[test]
    fn from_hex_accepts_uppercase_since_a_hand_edited_preset_may_carry_it() {
        let hash = ContentHash::of(b"case insensitivity");
        let upper = hash.to_string().to_ascii_uppercase();
        assert_eq!(ContentHash::from_hex(&upper), Ok(hash));
    }

    #[test]
    fn from_hex_rejects_wrong_length() {
        assert_eq!(
            ContentHash::from_hex("abcd"),
            Err(ContentHashParseError::WrongLength)
        );
        assert_eq!(
            ContentHash::from_hex(""),
            Err(ContentHashParseError::WrongLength)
        );
    }

    #[test]
    fn from_hex_rejects_non_hex_characters() {
        // 64 characters, but the last one is not a hex digit.
        let mut s = "a".repeat(63);
        s.push('z');
        assert_eq!(
            ContentHash::from_hex(&s),
            Err(ContentHashParseError::NotHex)
        );
    }

    #[test]
    fn from_bytes_is_the_inverse_of_as_bytes() {
        let hash = ContentHash::of(b"round trip via raw bytes");
        assert_eq!(ContentHash::from_bytes(*hash.as_bytes()), hash);
    }

    /// D-11.3's consequence note (§11): the hash → path map needs stable ordering, the same
    /// reason D-11.1 wants sorted JSON keys. `Ord` must exist and must be a total, consistent
    /// order — this pins that it does, rather than assuming the derive does the obviously right
    /// thing.
    #[test]
    fn ord_is_consistent_with_eq() {
        let a = ContentHash::from_bytes([0u8; 32]);
        let b = ContentHash::from_bytes([1u8; 32]);
        assert!(a < b);
        assert_eq!(a.cmp(&a), std::cmp::Ordering::Equal);
    }

    /// The streaming hasher must agree with `ContentHash::of` on the same bytes, however the
    /// input is chunked — this is what lets `namir-library`'s scan hash a file without holding
    /// the whole thing in memory at once.
    #[test]
    fn content_hasher_agrees_with_of_regardless_of_chunking() {
        let data = (0u32..10_000)
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<u8>>();
        let whole = ContentHash::of(&data);

        let mut hasher = ContentHasher::new();
        for chunk in data.chunks(777) {
            hasher.update(chunk);
        }
        assert_eq!(hasher.finish(), whole);
    }

    #[test]
    fn content_hasher_of_no_input_matches_of_empty_slice() {
        assert_eq!(ContentHasher::new().finish(), ContentHash::of(b""));
    }
}
