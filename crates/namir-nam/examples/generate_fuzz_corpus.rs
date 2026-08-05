//! Reproducible source of the seed corpus checked in at `fuzz/corpus/load_nam/` (D-19.1: every
//! automated fixture is *generated*, never captured or hand-crafted — this corpus is no
//! exception). Re-running this example regenerates byte-identical output: every input to this
//! generator is a fixed literal seed below, nothing here reaches for OS randomness, and
//! `namir_fixtures::nam::generate` / `namir_fixtures::mutate::seeded_corpus` are themselves
//! deterministic per-seed (see their own doc comments). Verified by hand: run, capture the
//! output, run again, `diff` — identical.
//!
//! One valid fixture (the smallest/fastest shape, [`WaveNetShape::Nano`], so libFuzzer's corpus
//! minimization and initial runs stay cheap) plus its [`namir_fixtures::mutate::seeded_corpus`]
//! variants (one per [`namir_fixtures::mutate::Mutation`] kind) give the fuzzer both a
//! structurally valid starting point and a set of near-valid, differently-broken ones to mutate
//! further from — the same "valid files as fuzz seeds, plus mutations" split D-19.1's robustness
//! row calls for.
//!
//! Usage: `cargo run -p namir-nam --example generate_fuzz_corpus`

use namir_fixtures::nam::{self, WaveNetShape};
use std::path::{Path, PathBuf};

/// Seeds the one valid fixture. Fixed and documented per D-19.1: this is what makes the corpus
/// reproducible from source rather than an opaque binary blob.
const VALID_SEED: u64 = 1;

/// Seeds the mutation pass over the valid fixture's bytes. Deliberately different from
/// `VALID_SEED` — no significance to the choice beyond being another fixed literal.
const MUTATION_SEED: u64 = 100;

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus/load_nam")
}

fn main() {
    let dir = corpus_dir();
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("failed to create corpus dir {}: {e}", dir.display()));

    let model = nam::generate(WaveNetShape::Nano, VALID_SEED)
        .unwrap_or_else(|e| panic!("fixture generation is degenerate: {e}"));
    let valid_bytes = model.to_json_bytes();

    let valid_path = dir.join("valid_nano.json");
    std::fs::write(&valid_path, &valid_bytes)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", valid_path.display()));
    println!(
        "wrote {} ({} bytes)",
        valid_path.display(),
        valid_bytes.len()
    );

    for (i, variant) in namir_fixtures::mutate::seeded_corpus(&valid_bytes, MUTATION_SEED)
        .into_iter()
        .enumerate()
    {
        let path = dir.join(format!("mutated_{i}.bin"));
        std::fs::write(&path, &variant)
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
        println!("wrote {} ({} bytes)", path.display(), variant.len());
    }
}
