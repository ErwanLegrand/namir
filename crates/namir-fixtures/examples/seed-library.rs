//! Writes a small, D-19.1-generated model+IR set into a directory you name — the fixture setup
//! `docs/manual-tests/fr-ui-020-single-screen-elements.md` and
//! `docs/manual-tests/fr-ui-070-non-modal-error-notices.md` need before either script can be run.
//!
//! Both scripts want a `.nam` and a `.wav` in the product's library root, and FR-UI-070's step 3
//! additionally wants an IR **longer than 10 seconds** so the load path reports
//! `worker.ir.truncated` (`namir_ir`'s `MAX_LOAD_SECONDS`, `crates/namir-ir/src/wav.rs`). Neither
//! [`library::generate_shared_corpus`]'s cached corpus nor [`library::mutable_probe_set`] contains
//! a file that long — every IR either generates is 1,024 samples — so it is generated here.
//!
//! Why an example rather than "copy two files out of the fixture cache", which is what those two
//! documents said until this existed: the cache tree is keyed by a content hash of the generator's
//! own constants, so its directory name changes whenever they do, and a human following a script
//! should not have to find it. Nothing here is captured audio (D-19.1) and nothing reaches for OS
//! randomness — same seed, same bytes.
//!
//! ```text
//! cargo run -p namir-fixtures --example seed-library -- <dir> [seed]
//! ```
//!
//! `<dir>` is the product's library root — `%APPDATA%\Namir\Library` on Windows,
//! `~/Library/Application Support/Namir/Library` on macOS,
//! `$XDG_CONFIG_HOME/namir/Library` on Linux (`namir_platform::config_dir` plus
//! `LibraryService::open_default`'s `Library` subdirectory). It is created if absent. `[seed]`
//! defaults to 1, matching the seed those two documents name.

use std::path::Path;
use std::process::ExitCode;

use namir_fixtures::ir;
use namir_fixtures::library;
use namir_fixtures::nam::{self, WaveNetShape};

/// The over-long IR's length, in seconds at [`OVERLONG_IR_RATE`]. Comfortably past `namir-ir`'s
/// 10-second ceiling, so a small change to either side of that comparison cannot make this file
/// silently stop exercising the truncation path it exists for.
const OVERLONG_IR_SECONDS: u32 = 12;
const OVERLONG_IR_RATE: u32 = 48_000;
/// A one-second decay envelope — long enough that the file is not effectively silence past its
/// first few milliseconds, which would make "the IR loaded and is audible" hard to judge by ear.
const OVERLONG_IR_TAU_SAMPLES: f64 = OVERLONG_IR_RATE as f64;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next() else {
        eprintln!(
            "usage: cargo run -p namir-fixtures --example seed-library -- <library-dir> [seed]\n\
             \n\
             <library-dir> is the product's library root:\n\
             \x20 Windows  %APPDATA%\\Namir\\Library\n\
             \x20 macOS    ~/Library/Application Support/Namir/Library\n\
             \x20 Linux    $XDG_CONFIG_HOME/namir/Library"
        );
        return ExitCode::from(2);
    };
    let seed: u64 = match args.next() {
        None => 1,
        Some(s) => match s.parse() {
            Ok(seed) => seed,
            Err(e) => {
                eprintln!("seed {s:?} is not a u64: {e}");
                return ExitCode::from(2);
            }
        },
    };

    let dir = Path::new(&dir);
    match seed_library(dir, seed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("failed to seed {}: {e}", dir.display());
            ExitCode::FAILURE
        }
    }
}

fn seed_library(dir: &Path, seed: u64) -> std::io::Result<()> {
    let probe = library::mutable_probe_set(dir, seed)?;
    println!(
        "{} files from mutable_probe_set(seed={seed}) -> {}",
        probe.entries.len(),
        dir.display()
    );

    // The probe set's models are all `WaveNetShape::Nano` cloned from one base model — right for
    // an index, thin for listening. `Standard` is the S-1 spike's verified shape, so the manual
    // scripts have one model whose output is worth judging by ear.
    let standard = nam::generate(WaveNetShape::Standard, seed)
        .expect("Standard is a calibrated shape and does not degenerate at any seed");
    let standard_path = dir.join("nam_standard.nam");
    std::fs::write(&standard_path, standard.to_json_bytes())?;
    println!("  {}  (WaveNetShape::Standard)", standard_path.display());

    let samples = ir::decaying_noise(
        (OVERLONG_IR_SECONDS * OVERLONG_IR_RATE) as usize,
        seed,
        OVERLONG_IR_TAU_SAMPLES,
    );
    let overlong_path = dir.join(format!("ir_overlong_{OVERLONG_IR_SECONDS}s.wav"));
    std::fs::write(
        &overlong_path,
        ir::to_mono_wav_bytes(&samples, OVERLONG_IR_RATE),
    )?;
    println!(
        "  {}  ({OVERLONG_IR_SECONDS} s at {OVERLONG_IR_RATE} Hz — FR-UI-070 step 3's \
         worker.ir.truncated induction)",
        overlong_path.display()
    );

    println!(
        "\nNext: keep a pristine copy of these outside the library root (several manual steps \
         corrupt or delete them), launch the product, and press Rescan library — the index only \
         picks up files present at scan time."
    );
    Ok(())
}
