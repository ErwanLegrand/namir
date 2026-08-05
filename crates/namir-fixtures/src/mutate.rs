//! Seeded fuzz-mutation utilities (D-19.1's robustness row: "Valid files as fuzz seeds, plus
//! mutations"). This is corpus seeding, not a fuzzer: it takes one known-valid `.nam` byte
//! buffer and returns deterministic variants for a fuzzer to explore from, deliberately simple —
//! finding *new* interesting inputs is the fuzzer's job, not this module's.

use rand::Rng;
use rand::SeedableRng;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
    /// Flips one random bit in one random byte. Length-preserving.
    ByteFlip,
    /// Cuts the buffer off at a random, non-zero offset. Always shorter (or equal, for empty
    /// input), never longer.
    Truncate,
    /// Parses as JSON and removes one random object field (from anywhere in the tree, not just
    /// the top level — an inner `layers[0].channels` is as good a target as a top-level key).
    /// Falls back to a byte flip if the buffer doesn't parse as a JSON object.
    DropField,
    /// Parses as JSON and corrupts one random numeric leaf (negate, zero, or scale it by a huge
    /// factor). Falls back to a byte flip if the buffer doesn't parse as JSON or has no numbers.
    CorruptNumber,
}

/// All four kinds, in a stable order — useful for building a corpus that covers each once.
pub const ALL: [Mutation; 4] = [Mutation::ByteFlip, Mutation::Truncate, Mutation::DropField, Mutation::CorruptNumber];

/// Applies `mutation` to `data` with a seeded RNG; same `(data, mutation, seed)` always produces
/// the same output.
pub fn mutate(data: &[u8], mutation: Mutation, seed: u64) -> Vec<u8> {
    let mut rng = rand_pcg::Pcg64::seed_from_u64(seed);
    match mutation {
        Mutation::ByteFlip => byte_flip(data, &mut rng),
        Mutation::Truncate => truncate(data, &mut rng),
        Mutation::DropField => drop_field(data, &mut rng),
        Mutation::CorruptNumber => corrupt_number(data, &mut rng),
    }
}

/// Generates one mutated variant per entry in [`ALL`], seeded from `seed + index` so the corpus
/// is reproducible but each member is distinct even when the underlying `data` is small enough
/// that two mutation kinds might otherwise collide.
pub fn seeded_corpus(data: &[u8], seed: u64) -> Vec<Vec<u8>> {
    ALL.iter().enumerate().map(|(i, &m)| mutate(data, m, seed.wrapping_add(i as u64))).collect()
}

fn byte_flip(data: &[u8], rng: &mut impl Rng) -> Vec<u8> {
    let mut out = data.to_vec();
    if out.is_empty() {
        return out;
    }
    let idx = rng.gen_range(0..out.len());
    let bit = rng.gen_range(0..8u8);
    out[idx] ^= 1 << bit;
    out
}

fn truncate(data: &[u8], rng: &mut impl Rng) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    // Range is 0..len (exclusive of the full length) so this always removes at least one byte —
    // a truncation that returns the input unchanged wouldn't be testing truncation handling.
    let cut = rng.gen_range(0..data.len());
    data[..cut].to_vec()
}

/// Walks a JSON value, collecting every `(container, key)` pair addressable for field removal —
/// `container` is a JSON Pointer (RFC 6901) to the object that owns `key`. Recurses into arrays
/// too (indices become pointer segments) so a field nested inside `config.layers[1]` is as
/// reachable as a top-level one.
fn collect_object_keys(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                out.push((path.to_string(), k.clone()));
                collect_object_keys(v, &format!("{path}/{k}"), out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                collect_object_keys(v, &format!("{path}/{i}"), out);
            }
        }
        _ => {}
    }
}

/// Walks a JSON value, collecting a JSON Pointer to every numeric leaf.
fn collect_number_paths(value: &Value, path: &str, out: &mut Vec<String>) {
    match value {
        Value::Number(_) => out.push(path.to_string()),
        Value::Object(map) => {
            for (k, v) in map {
                collect_number_paths(v, &format!("{path}/{k}"), out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                collect_number_paths(v, &format!("{path}/{i}"), out);
            }
        }
        _ => {}
    }
}

fn drop_field(data: &[u8], rng: &mut impl Rng) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<Value>(data) else {
        return byte_flip(data, rng);
    };
    let mut keys = Vec::new();
    collect_object_keys(&value, "", &mut keys);
    if keys.is_empty() {
        return byte_flip(data, rng);
    }
    let (container_ptr, key) = &keys[rng.gen_range(0..keys.len())];
    let container = if container_ptr.is_empty() {
        Some(&mut value)
    } else {
        value.pointer_mut(container_ptr)
    };
    if let Some(Value::Object(map)) = container {
        map.remove(key);
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| data.to_vec())
}

fn corrupt_number(data: &[u8], rng: &mut impl Rng) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<Value>(data) else {
        return byte_flip(data, rng);
    };
    let mut paths = Vec::new();
    collect_number_paths(&value, "", &mut paths);
    if paths.is_empty() {
        return byte_flip(data, rng);
    }
    let path = &paths[rng.gen_range(0..paths.len())];
    let original = value.pointer(path).and_then(Value::as_f64);
    let corrupted = original.map(|v| match rng.gen_range(0..3u8) {
        0 => -v,
        1 => 0.0,
        _ => v * 1e9,
    });
    if let (Some(v), Some(slot)) = (corrupted, value.pointer_mut(path)) {
        *slot = serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number);
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_nam_json() -> Vec<u8> {
        serde_json::json!({
            "architecture": "WaveNet",
            "config": {
                "layers": [
                    {"channels": 16, "kernel_size": 3, "dilations": [1, 2, 4]}
                ],
                "head_scale": 0.02
            },
            "weights": [0.1, 0.2, -0.3, 0.4],
            "sample_rate": 48000
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn byte_flip_changes_exactly_one_bit_and_preserves_length() {
        let data = sample_nam_json();
        let mutated = mutate(&data, Mutation::ByteFlip, 1);
        assert_eq!(data.len(), mutated.len());
        let diff_bits: u32 = data.iter().zip(&mutated).map(|(a, b)| (a ^ b).count_ones()).sum();
        assert_eq!(diff_bits, 1);
    }

    #[test]
    fn byte_flip_on_empty_input_does_not_panic() {
        assert_eq!(mutate(&[], Mutation::ByteFlip, 1), Vec::<u8>::new());
    }

    #[test]
    fn truncate_produces_a_strict_prefix() {
        let data = sample_nam_json();
        let mutated = mutate(&data, Mutation::Truncate, 1);
        assert!(mutated.len() < data.len());
        assert_eq!(&data[..mutated.len()], mutated.as_slice());
    }

    #[test]
    fn truncate_on_empty_input_does_not_panic() {
        assert_eq!(mutate(&[], Mutation::Truncate, 1), Vec::<u8>::new());
    }

    #[test]
    fn drop_field_removes_a_key_that_was_present() {
        let data = sample_nam_json();
        let mutated = mutate(&data, Mutation::DropField, 1);
        let original: Value = serde_json::from_slice(&data).unwrap();
        let after: Value = serde_json::from_slice(&mutated).unwrap();
        assert_ne!(original, after, "expected the mutated JSON to differ from the original");
    }

    #[test]
    fn drop_field_can_reach_nested_fields() {
        // With enough distinct seeds, at least one should hit something inside `config.layers`
        // rather than only ever removing a top-level key.
        let data = sample_nam_json();
        let hit_nested = (0..50).any(|seed| {
            let mutated = mutate(&data, Mutation::DropField, seed);
            let after: Value = serde_json::from_slice(&mutated).unwrap();
            after.pointer("/config/layers/0/channels").is_none()
                || after.pointer("/config/layers/0/kernel_size").is_none()
        });
        assert!(hit_nested, "expected at least one seed (of 50) to drop a nested field");
    }

    #[test]
    fn corrupt_number_changes_a_numeric_value() {
        let data = sample_nam_json();
        let mutated = mutate(&data, Mutation::CorruptNumber, 1);
        let original: Value = serde_json::from_slice(&data).unwrap();
        let after: Value = serde_json::from_slice(&mutated).unwrap();
        assert_ne!(original, after);
    }

    #[test]
    fn non_json_input_falls_back_to_byte_flip_for_structural_mutations() {
        let data = b"not json at all".to_vec();
        for m in [Mutation::DropField, Mutation::CorruptNumber] {
            let mutated = mutate(&data, m, 1);
            assert_eq!(mutated.len(), data.len());
            assert_ne!(mutated, data);
        }
    }

    #[test]
    fn mutations_are_deterministic_for_a_given_seed() {
        let data = sample_nam_json();
        for m in ALL {
            let a = mutate(&data, m, 42);
            let b = mutate(&data, m, 42);
            assert_eq!(a, b, "{m:?} was not deterministic");
        }
    }

    #[test]
    fn seeded_corpus_has_one_entry_per_mutation_kind() {
        let data = sample_nam_json();
        let corpus = seeded_corpus(&data, 5);
        assert_eq!(corpus.len(), ALL.len());
    }
}
