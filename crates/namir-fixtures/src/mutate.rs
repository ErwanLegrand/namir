//! Seeded fuzz-mutation utilities (D-19.1's robustness row: "Valid files as fuzz seeds, plus
//! mutations"). This is corpus seeding, not a fuzzer: it takes one known-valid `.nam` byte
//! buffer and returns deterministic variants for a fuzzer to explore from, deliberately simple —
//! finding *new* interesting inputs is the fuzzer's job, not this module's.

use rand::Rng;
use rand::SeedableRng;
use serde_json::Value;

/// One deterministic corpus-seeding mutation `mutate` can apply to a valid `.nam` byte buffer.
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
    /// Parses as JSON and replaces one random object field's *value* with `null`, leaving the key
    /// present. Falls back to a byte flip if the buffer doesn't parse as a JSON object.
    ///
    /// This is the shape of the real post-M6 `.nam` parser bug (a metadata field exported as
    /// JSON `null` rather than omitted): [`Mutation::DropField`] removes the key entirely, which
    /// a `#[serde(default)]`/`Option` field absorbs, and [`Mutation::CorruptNumber`] only ever
    /// rewrites a number as another number — neither can ever produce `"name": null`, so before
    /// this variant existed no generated fixture and no seeded fuzz corpus entry reached that
    /// region at all.
    NullField,
    /// Parses as JSON and replaces one random object field's *value* with a value of a different
    /// JSON type (a string where a number was, a number where a string was, a scalar where a
    /// container was). Falls back to a byte flip if the buffer doesn't parse as a JSON object.
    ///
    /// The type-confusion sibling of [`Mutation::NullField`]: a deserializer that is careful
    /// about missing fields and out-of-range numbers can still be careless about a field whose
    /// type is simply wrong.
    RetypeField,
}

/// All six kinds, in a stable order — useful for building a corpus that covers each once. The
/// order is append-only on purpose: [`seeded_corpus`] derives each variant's seed from its index
/// here, so inserting a kind anywhere but the end would silently change every later variant's
/// bytes (and therefore every checked-in fuzz corpus file generated from it).
pub const ALL: [Mutation; 6] = [
    Mutation::ByteFlip,
    Mutation::Truncate,
    Mutation::DropField,
    Mutation::CorruptNumber,
    Mutation::NullField,
    Mutation::RetypeField,
];

/// Applies `mutation` to `data` with a seeded RNG; same `(data, mutation, seed)` always produces
/// the same output.
pub fn mutate(data: &[u8], mutation: Mutation, seed: u64) -> Vec<u8> {
    let mut rng = rand_pcg::Pcg64::seed_from_u64(seed);
    match mutation {
        Mutation::ByteFlip => byte_flip(data, &mut rng),
        Mutation::Truncate => truncate(data, &mut rng),
        Mutation::DropField => drop_field(data, &mut rng),
        Mutation::CorruptNumber => corrupt_number(data, &mut rng),
        Mutation::NullField => null_field(data, &mut rng),
        Mutation::RetypeField => retype_field(data, &mut rng),
    }
}

/// Generates one mutated variant per entry in [`ALL`], seeded from `seed + index` so the corpus
/// is reproducible but each member is distinct even when the underlying `data` is small enough
/// that two mutation kinds might otherwise collide.
pub fn seeded_corpus(data: &[u8], seed: u64) -> Vec<Vec<u8>> {
    ALL.iter()
        .enumerate()
        .map(|(i, &m)| mutate(data, m, seed.wrapping_add(i as u64)))
        .collect()
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

/// Appends one segment to a JSON Pointer, escaping it per RFC 6901 §3: `~` becomes `~0` and `/`
/// becomes `~1`, in that order (reversing the order would re-escape the `~` the second rule just
/// introduced).
///
/// Not a formality. Every pointer in this module is built by string concatenation and then handed
/// to `serde_json`'s `pointer`/`pointer_mut`, which un-escape what they are given — so an
/// unescaped `/` inside a key silently *splits* the segment and the lookup resolves to `None`.
/// `drop_field`, `corrupt_number`, `null_field` and `retype_field` all treat `None` as "nothing to
/// do" and return the input, which puts a byte-identical duplicate of the seed file into the fuzz
/// corpus in place of a mutant: a mutation kind that appears to have run and did nothing. Keys
/// carrying `/` or `~` are reachable in real input — `.nam` files pass training metadata through
/// verbatim — so this is a live case, not a theoretical one.
fn push_pointer_segment(path: &str, segment: &str) -> String {
    let mut out = String::with_capacity(path.len() + segment.len() + 1);
    out.push_str(path);
    out.push('/');
    for ch in segment.chars() {
        match ch {
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            _ => out.push(ch),
        }
    }
    out
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
                collect_object_keys(v, &push_pointer_segment(path, k), out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                collect_object_keys(v, &push_pointer_segment(path, &i.to_string()), out);
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
                collect_number_paths(v, &push_pointer_segment(path, k), out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                collect_number_paths(v, &push_pointer_segment(path, &i.to_string()), out);
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

/// The object that owns `container_ptr`, as a mutable map — the shared half of `drop_field`,
/// `null_field` and `retype_field`'s "reach into the tree and edit one field" step. An empty
/// pointer addresses the document root.
fn object_mut<'a>(
    value: &'a mut Value,
    container_ptr: &str,
) -> Option<&'a mut serde_json::Map<String, Value>> {
    let container = if container_ptr.is_empty() {
        Some(value)
    } else {
        value.pointer_mut(container_ptr)
    };
    match container {
        Some(Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// The current value of `container_ptr`'s `key` field, addressed the same way
/// [`collect_object_keys`] built the pointer.
fn child<'a>(value: &'a Value, container_ptr: &str, key: &str) -> Option<&'a Value> {
    value.pointer(&push_pointer_segment(container_ptr, key))
}

/// Replaces one random object field's value with `null`, keeping the key. See
/// [`Mutation::NullField`] for why this is a distinct kind rather than a case of `DropField`.
fn null_field(data: &[u8], rng: &mut impl Rng) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<Value>(data) else {
        return byte_flip(data, rng);
    };
    let mut keys = Vec::new();
    collect_object_keys(&value, "", &mut keys);
    // Writing `null` over a field that is already `null` would be a no-op "mutation" that emits
    // the input unchanged — never a useful corpus entry.
    keys.retain(|(container, key)| !matches!(child(&value, container, key), Some(Value::Null)));
    if keys.is_empty() {
        return byte_flip(data, rng);
    }
    let (container_ptr, key) = keys[rng.gen_range(0..keys.len())].clone();
    if let Some(map) = object_mut(&mut value, &container_ptr) {
        map.insert(key, Value::Null);
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| data.to_vec())
}

/// The wrong-typed replacement for `value`: a string where a number was, a number where a string
/// was, a scalar where a container was. Deterministic given the field that was picked — the RNG's
/// only job in [`retype_field`] is choosing *which* field to hit.
fn retyped(value: &Value) -> Value {
    match value {
        Value::Null => Value::Bool(true),
        Value::Bool(_) => Value::String("true".to_string()),
        Value::Number(n) => Value::String(n.to_string()),
        Value::String(_) => Value::Number(0.into()),
        Value::Array(_) | Value::Object(_) => Value::Number(0.into()),
    }
}

/// Replaces one random object field's value with a value of a different JSON type, keeping the
/// key. See [`Mutation::RetypeField`].
fn retype_field(data: &[u8], rng: &mut impl Rng) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<Value>(data) else {
        return byte_flip(data, rng);
    };
    let mut keys = Vec::new();
    collect_object_keys(&value, "", &mut keys);
    if keys.is_empty() {
        return byte_flip(data, rng);
    }
    let (container_ptr, key) = keys[rng.gen_range(0..keys.len())].clone();
    let Some(replacement) = child(&value, &container_ptr, &key).map(retyped) else {
        return byte_flip(data, rng);
    };
    if let Some(map) = object_mut(&mut value, &container_ptr) {
        map.insert(key, replacement);
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
        let diff_bits: u32 = data
            .iter()
            .zip(&mutated)
            .map(|(a, b)| (a ^ b).count_ones())
            .sum();
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

    /// A key containing `/` or `~` used to defeat every JSON-aware mutation kind: the pointer to
    /// its container was built by raw concatenation, `serde_json` un-escaped it back into a
    /// different path, the lookup missed, and `drop_field`/`corrupt_number`/`null_field`/
    /// `retype_field` all returned the input **unchanged** — writing a byte-identical copy of the
    /// seed file into the corpus as if it were a mutant. Both special characters are reachable in
    /// real input, since `.nam` files carry exporter training-metadata keys through verbatim.
    ///
    /// Every addressable field in this document sits behind such a key, so *whichever* field a
    /// given seed picks, the mutation has to change something. Swept over many seeds rather than
    /// pinned to one: which field a seed reaches is an implementation detail, "no seed produces a
    /// no-op" is the property.
    #[test]
    fn a_key_containing_a_slash_or_a_tilde_is_still_mutable() {
        let data = serde_json::json!({
            "a/b": {"c~d": 1, "e/~f": [2, 3]},
            "g~1h": {"i//j": 4}
        })
        .to_string()
        .into_bytes();

        for mutation in [
            Mutation::DropField,
            Mutation::CorruptNumber,
            Mutation::NullField,
            Mutation::RetypeField,
        ] {
            for seed in 0..40u64 {
                let mutated = mutate(&data, mutation, seed);
                assert_ne!(
                    mutated, data,
                    "{mutation:?} with seed {seed} silently returned its input unchanged: the \
                     JSON Pointer for a key containing `/` or `~` did not resolve"
                );
                // The fallback byte flip would also change the bytes -- but only by corrupting
                // the JSON. These four kinds are meant to produce a structurally *valid*
                // document with one field changed, which is what makes them different seeds for
                // a fuzzer than `ByteFlip` already is.
                serde_json::from_slice::<Value>(&mutated).unwrap_or_else(|e| {
                    panic!("{mutation:?} with seed {seed} fell back to a byte flip: {e}")
                });
            }
        }
    }

    #[test]
    fn pointer_segments_are_escaped_per_rfc_6901() {
        assert_eq!(push_pointer_segment("", "plain"), "/plain");
        assert_eq!(push_pointer_segment("/a", "b/c"), "/a/b~1c");
        assert_eq!(push_pointer_segment("/a", "b~c"), "/a/b~0c");
        // `~` first, then `/`: escaping in the other order would turn `~` into `~01`.
        assert_eq!(push_pointer_segment("", "~/"), "/~0~1");
    }

    #[test]
    fn drop_field_removes_a_key_that_was_present() {
        let data = sample_nam_json();
        let mutated = mutate(&data, Mutation::DropField, 1);
        let original: Value = serde_json::from_slice(&data).unwrap();
        let after: Value = serde_json::from_slice(&mutated).unwrap();
        assert_ne!(
            original, after,
            "expected the mutated JSON to differ from the original"
        );
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
        assert!(
            hit_nested,
            "expected at least one seed (of 50) to drop a nested field"
        );
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
        for m in [
            Mutation::DropField,
            Mutation::CorruptNumber,
            Mutation::NullField,
            Mutation::RetypeField,
        ] {
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

    /// Every `(container, key)` pair in `value` whose value is JSON `null`, as a set of
    /// `"/container/key"` pointers — the shape the real post-M6 parser bug had.
    fn null_valued_pointers(value: &Value) -> Vec<String> {
        let mut keys = Vec::new();
        collect_object_keys(value, "", &mut keys);
        keys.into_iter()
            .filter(|(c, k)| matches!(child(value, c, k), Some(Value::Null)))
            .map(|(c, k)| format!("{c}/{k}"))
            .collect()
    }

    #[test]
    fn null_field_writes_json_null_into_a_slot_that_held_a_string() {
        // `architecture` is the sample document's only string field, so "some seed nulls a
        // string" is checkable exactly rather than statistically.
        let data = sample_nam_json();
        let hit = (0..50u64).any(|seed| {
            let mutated = mutate(&data, Mutation::NullField, seed);
            let after: Value = serde_json::from_slice(&mutated).expect("still valid JSON");
            after.pointer("/architecture") == Some(&Value::Null)
        });
        assert!(
            hit,
            "expected at least one seed (of 50) to null the `architecture` string"
        );
    }

    #[test]
    fn null_field_keeps_the_key_and_never_merely_re_nulls_an_existing_null() {
        let data = sample_nam_json();
        let original: Value = serde_json::from_slice(&data).unwrap();
        assert!(
            null_valued_pointers(&original).is_empty(),
            "the sample document is expected to start with no nulls at all"
        );
        for seed in 0..25u64 {
            let mutated = mutate(&data, Mutation::NullField, seed);
            let after: Value = serde_json::from_slice(&mutated).expect("still valid JSON");
            assert_ne!(original, after, "seed {seed}: NullField was a no-op");
            assert_eq!(
                null_valued_pointers(&after).len(),
                1,
                "seed {seed}: expected exactly one field to become null"
            );
        }
    }

    #[test]
    fn retype_field_replaces_one_value_with_a_different_json_type() {
        let data = sample_nam_json();
        let original: Value = serde_json::from_slice(&data).unwrap();
        let mut seen_number_as_string = false;
        let mut seen_string_as_number = false;
        for seed in 0..50u64 {
            let mutated = mutate(&data, Mutation::RetypeField, seed);
            let after: Value = serde_json::from_slice(&mutated).expect("still valid JSON");
            assert_ne!(original, after, "seed {seed}: RetypeField was a no-op");
            if after.pointer("/sample_rate").is_some_and(Value::is_string) {
                seen_number_as_string = true;
            }
            if after.pointer("/architecture").is_some_and(Value::is_number) {
                seen_string_as_number = true;
            }
        }
        assert!(
            seen_number_as_string,
            "expected some seed to put a string where `sample_rate`'s number was"
        );
        assert!(
            seen_string_as_number,
            "expected some seed to put a number where `architecture`'s string was"
        );
    }

    /// The regression this whole pair of mutation kinds exists for: a corpus seeded from a *real*
    /// generated `.nam` fixture must be able to produce the shape of the real post-M6 parser bug
    /// — a metadata field present but set to JSON `null` rather than omitted. Before `NullField`
    /// existed this was unreachable: `DropField` removes the key and `CorruptNumber` only ever
    /// rewrites a number as another number, so no seed of any count could have passed this.
    #[test]
    fn a_seeded_corpus_of_a_generated_fixture_reaches_a_null_metadata_field() {
        let model = crate::nam::generate(crate::nam::WaveNetShape::Nano, 1)
            .expect("nano fixture should generate");
        let bytes = model.to_json_bytes();

        let metadata_fields = [
            "name",
            "modeled_by",
            "gear_type",
            "tone_type",
            "description",
        ];
        let hit = (0..60u64).any(|seed| {
            seeded_corpus(&bytes, seed).iter().any(|variant| {
                let Ok(after) = serde_json::from_slice::<Value>(variant) else {
                    return false;
                };
                metadata_fields
                    .iter()
                    .any(|f| after.pointer(&format!("/metadata/{f}")) == Some(&Value::Null))
            })
        });
        assert!(
            hit,
            "no seed produced a `\"metadata.<field>\": null` document — the exact shape of the \
             real post-M6 parser bug"
        );
    }
}
