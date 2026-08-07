//! Reproducible source of the seed corpus checked in at `fuzz/corpus/probe_wav/` (D-19.1: every
//! automated fixture is *generated*, never captured or hand-crafted -- this corpus is no
//! exception, mirroring `namir-nam/examples/generate_fuzz_corpus.rs`'s identical structure and
//! rationale). Named `generate_ir_fuzz_corpus` rather than reusing that file's own name
//! (`generate_fuzz_corpus`) for the same reason `namir-state`'s corpus generator already is (see
//! that file's own doc comment): two example binaries with the same name across different crates
//! in one workspace collide on their output filename -- a hard `LNK1104` link failure on Windows.
//! Re-running this example regenerates byte-identical output: `VALID_FRAMES`/`MUTATION_SEED` below
//! are the only inputs, `namir_fixtures::ir::delta`/`to_mono_wav_bytes` and
//! `namir_fixtures::mutate::seeded_corpus` are themselves deterministic (see their own doc
//! comments), and none of this reaches for OS randomness.
//!
//! One valid WAV (a short mono 16-bit-PCM delta impulse, encoded via
//! `namir_fixtures::ir::to_mono_wav_bytes` -- the same generator `namir-ir`'s own correctness
//! tests already trust) plus its [`namir_fixtures::mutate::seeded_corpus`] variants. That mutator
//! is JSON-aware for two of its four kinds (`DropField`/`CorruptNumber`) and falls back to a byte
//! flip on non-JSON input (its own doc comment) -- WAV bytes always take that fallback path for
//! those two, which is expected and still yields four distinct near-valid variants overall
//! (`ByteFlip`/`Truncate` operate on raw bytes regardless of format).
//!
//! Usage: `cargo run -p namir-ir --example generate_ir_fuzz_corpus`

use namir_fixtures::ir;
use std::path::{Path, PathBuf};

/// Length (in frames) of the seed impulse. Fixed and documented per D-19.1: this is what makes
/// the corpus reproducible from source rather than an opaque binary blob.
const VALID_FRAMES: usize = 64;

/// Seeds the mutation pass over the valid fixture's bytes. Deliberately different from
/// `VALID_FRAMES`'s role -- no significance to the choice beyond being another fixed literal.
const MUTATION_SEED: u64 = 100;

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus/probe_wav")
}

fn main() {
    let dir = corpus_dir();
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("failed to create corpus dir {}: {e}", dir.display()));

    let samples = ir::delta(VALID_FRAMES);
    let valid_bytes = ir::to_mono_wav_bytes(&samples, 48_000);

    let valid_path = dir.join("valid_delta.wav");
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
