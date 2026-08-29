//! `cargo run -p xtask -- preset [output-path]`: writes a sample `.namirpreset` document.
//!
//! M5 builds no product shell (namir-app/namir-clap are M6+), so there is no UI path that can
//! produce a real preset file for a human to inspect. `docs/manual-tests/`'s FR-STATE-040 and
//! NFR-DOC-010 scripts both need one to exist without reading Rust source to get it — this is
//! that non-UI path, the same role the S-1 spike's `generate_fixture.rs` played for `.nam` files
//! before `namir-nam` had a real writer.
//!
//! The sample is deliberately not run through `namir-state`'s embedded-data path (FR-STATE-080):
//! an embedded base64 blob would make the file harder to read and hand-edit, which is exactly the
//! opposite of what a document meant to demonstrate diffability and hand-editability should do.
//! Both references use plausible library-relative and absolute paths instead.

use std::path::Path;

use namir_core::ContentHash;
use namir_state::{FileRef, RelPath, State};

fn sample_state() -> State {
    let mut state = State::defaults();
    state
        .params
        .set("trim.gain_db", 3.0)
        .expect("trim.gain_db is a real parameter key");
    state
        .params
        .set("eq.mid_q", 0.7)
        .expect("eq.mid_q is a real parameter key");
    state
        .params
        .set("ir.level_db", -3.0)
        .expect("ir.level_db is a real parameter key");
    state.set_global_bypass(false);
    state.set_output_ceiling_db(0.0);
    state.nam = Some(FileRef {
        hash: ContentHash::of(b"xtask preset sample -- not a real model file"),
        library_relative: Some(RelPath::parse("marshall/plexi.nam").expect("well-formed")),
        absolute: Some("C:\\Users\\erwan\\Models\\marshall\\plexi.nam".to_string()),
        display_name: "plexi.nam".to_string(),
        embedded: None,
    });
    state.ir = Some(FileRef {
        hash: ContentHash::of(b"xtask preset sample -- not a real IR file"),
        library_relative: Some(RelPath::parse("cabs/1960a.wav").expect("well-formed")),
        absolute: Some("/home/erwan/irs/1960a.wav".to_string()),
        display_name: "1960a.wav".to_string(),
        embedded: None,
    });
    state
}

/// Runs the subcommand. `args` is everything after `preset` on the command line.
///
/// `preset [output-path]` writes the sample document (to `output-path`, or to stdout with no
/// path — `cargo run -p xtask -- preset > sample.namirpreset` works the same as passing the path
/// directly).
///
/// `preset --verify <path>` reads `path` back through `namir_state::State::read` and prints the
/// parameters/references it found, plus any warnings — the read half of FR-STATE-040's manual
/// test (docs/manual-tests/): a document hand-edited after `preset` wrote it must still load, and
/// the edit must actually take effect.
/// The sample document's bytes, so `xtask schema`'s default document set can include the one
/// document this tool itself produces -- the artifact `docs/manual-tests/`'s FR-STATE-040 script
/// hands a human to inspect and hand-edit, which had better conform to the format that script is
/// demonstrating.
pub fn sample_bytes() -> Vec<u8> {
    sample_state().write()
}

pub fn run(args: &[String]) -> bool {
    if args.first().map(String::as_str) == Some("--verify") {
        let Some(path_str) = args.get(1) else {
            println!("preset --verify: missing <path>");
            return false;
        };
        return verify(Path::new(path_str));
    }

    let bytes = sample_state().write();
    match args.first() {
        Some(path_str) => match std::fs::write(Path::new(path_str), &bytes) {
            Ok(()) => {
                println!("preset: wrote {path_str} ({} bytes)", bytes.len());
                true
            }
            Err(e) => {
                println!("preset: could not write {path_str}: {e}");
                false
            }
        },
        None => {
            print!("{}", String::from_utf8_lossy(&bytes));
            true
        }
    }
}

fn verify(path: &Path) -> bool {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            println!("preset --verify: could not read {}: {e}", path.display());
            return false;
        }
    };
    match State::read(&bytes) {
        Ok((state, warnings)) => {
            println!("preset --verify: {} loaded successfully", path.display());
            println!("  global.bypass = {}", state.global_bypass());
            println!("  global.output_ceiling_db = {}", state.output_ceiling_db());
            println!("  trim.gain_db = {:?}", state.params.get("trim.gain_db"));
            println!("  eq.mid_q = {:?}", state.params.get("eq.mid_q"));
            println!("  ir.level_db = {:?}", state.params.get("ir.level_db"));
            println!("  nam reference = {:?}", state.nam.map(|r| r.display_name));
            println!("  ir reference = {:?}", state.ir.map(|r| r.display_name));
            if warnings.is_empty() {
                println!("  warnings: none");
            } else {
                for w in &warnings {
                    println!("  warning: {w}");
                }
            }
            true
        }
        Err(e) => {
            println!("preset --verify: {} failed to load: {e}", path.display());
            false
        }
    }
}
