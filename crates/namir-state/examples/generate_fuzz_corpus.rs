//! Reproducible source of the seed corpus checked in at `fuzz/corpus/read_state/` (D-19.1: every
//! automated fixture is *generated*, never captured or hand-crafted — this corpus is no
//! exception, mirroring `namir-nam/examples/generate_fuzz_corpus.rs`'s identical structure and
//! rationale). Re-running this example regenerates byte-identical output: `MUTATION_SEED` below
//! is the only input, `State::defaults`/`write` and `namir_fixtures::mutate::seeded_corpus` are
//! themselves deterministic (see their own doc comments), and `State::defaults` reaches for no OS
//! randomness at all. Verified by hand: run, capture the output, run again, `diff` — identical.
//!
//! One valid document — `State::defaults()` with a couple of non-default fields set (a parameter,
//! `global.bypass`, and a `FileRef` with an embedded copy) so the `references` and `global`
//! sections are actually present for the mutator to target, not merely `parameters` — plus its
//! [`namir_fixtures::mutate::seeded_corpus`] variants (one per
//! [`namir_fixtures::mutate::Mutation`] kind). The mutator is format-agnostic (it walks generic
//! JSON), so it needs no changes to seed a *second* JSON reader's corpus, which is D-11.1's whole
//! reason for choosing one format across every consumer (P6) — this file is that reason paying
//! off in code, not just prose.
//!
//! Usage: `cargo run -p namir-state --example generate_fuzz_corpus`

use std::path::{Path, PathBuf};

use namir_core::ContentHash;
use namir_state::{EmbeddedRef, FileRef, State};

/// Seeds the mutation pass over the valid document's bytes. Fixed and documented per D-19.1: this
/// is what makes the corpus reproducible from source rather than an opaque binary blob.
const MUTATION_SEED: u64 = 100;

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus/read_state")
}

fn valid_state() -> State {
    let mut state = State::defaults();
    state
        .params
        .set("trim.gain_db", 3.0)
        .expect("trim.gain_db is a real REGISTRY key");
    state.global.bypass = true;
    state.global.output_ceiling_db = -1.0;
    let embedded_bytes = b"{\"fake\": \"minimal nam-shaped json for corpus seeding\"}".to_vec();
    state.nam = Some(FileRef {
        hash: ContentHash::of(&embedded_bytes),
        library_relative: None,
        absolute: None,
        display_name: "seed.nam".to_string(),
        embedded: Some(EmbeddedRef {
            media_type: "application/vnd.namir.nam+json".to_string(),
            data: embedded_bytes,
        }),
    });
    state
}

fn main() {
    let dir = corpus_dir();
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("failed to create corpus dir {}: {e}", dir.display()));

    let valid_bytes = valid_state().write();

    let valid_path = dir.join("valid_state.namirpreset");
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
