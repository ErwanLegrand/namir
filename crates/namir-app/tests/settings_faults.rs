//! FR-ERR-040's settings-I/O subsystem: "inject a fault into each non-audio subsystem".
//!
//! The requirement's method quantifies over subsystems, and settings I/O is one of them.
//! `namir-worker`'s `tests/fault_injection.rs` injects a fault into every non-audio subsystem
//! that crate can see — the pool, the library index store, the scanner, the resource cache, the
//! resource-load path, state document parsing and preset recall — and states in its own doc
//! comment why settings I/O is not among them: `crate::settings` lives here, and D-5.1 runs the
//! `namir-app` -> `namir-worker` edge the other way, so `namir-worker` cannot see it. This file is
//! that subsystem's half, kept beside the code it exercises.
//!
//! `settings::load` is documented "never fails (P8)", which is FR-ERR-040's containment stated as
//! a function contract; what is checked here is that the contract holds against real injected
//! faults rather than only against the happy path, and that what comes back out is
//! catalogue-coded (FR-ERR-020) so the failure is reportable rather than merely survived.
//!
//! No `// trace:` tag: the covering artifact for FR-ERR-040 is
//! `namir-worker/tests/fault_injection.rs`, whose `trace-partial` names what is still unreached.
//! Two tags for one requirement would make the generated plan's component attribution say the
//! requirement is covered twice over rather than once across two crates.

use std::path::{Path, PathBuf};

use namir_app::settings::{self, AppSettings};

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "namir-app-settings-faults-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

fn assert_catalogued(label: &str, warning: &settings::SettingsWarning) {
    assert!(
        !warning.code.id.is_empty() && warning.code.id.contains('.'),
        "{label}: the contained fault produced an uncatalogued error id {:?}",
        warning.code.id
    );
}

#[test]
fn a_corrupt_settings_file_degrades_to_defaults_with_a_catalogued_warning() {
    let dir = scratch("corrupt");
    let path = settings::settings_path(&dir);

    for (label, bytes) in [
        ("not JSON at all", b"\x00\x01\x02 not json".as_slice()),
        ("JSON of the wrong shape", b"[1, 2, 3]".as_slice()),
        (
            "truncated mid-object",
            br#"{"sample_rate_hz": 48"#.as_slice(),
        ),
        ("empty", b"".as_slice()),
    ] {
        std::fs::write(&path, bytes).expect("write the corrupt fixture");
        let (loaded, warning) = settings::load(&path);
        let warning =
            warning.unwrap_or_else(|| panic!("{label}: must degrade with a warning, not silently"));
        assert_catalogued(label, &warning);
        assert_eq!(
            loaded,
            AppSettings::default(),
            "{label}: a corrupt file must degrade to defaults, not to whatever half-parsed"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_settings_path_that_is_a_directory_is_contained_in_both_directions() {
    let dir = scratch("directory");
    // The fault: the path settings live at is a directory. Reading it fails with something other
    // than NotFound, and writing to it fails at the rename -- both of which `load`/`save` have to
    // contain rather than panic on.
    let path = settings::settings_path(&dir);
    std::fs::create_dir_all(&path).expect("create the directory posing as the settings file");

    let (loaded, warning) = settings::load(&path);
    let warning = warning.expect("reading a directory must warn rather than succeed silently");
    assert_catalogued("path is a directory", &warning);
    assert_eq!(loaded, AppSettings::default());

    let err = settings::save(&path, &AppSettings::default())
        .expect_err("writing over a directory must fail rather than panic");
    assert_catalogued("save over a directory", &err);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_settings_file_is_the_first_run_case_and_warns_about_nothing() {
    // The control for the two tests above: containment must not mean "warn about everything".
    // A first launch has no settings file, and reporting that as a fault would train a user to
    // ignore the report FR-ERR-040 exists to make useful.
    let dir = scratch("missing");
    let path = settings::settings_path(&dir);
    assert!(!Path::new(&path).exists());

    let (loaded, warning) = settings::load(&path);
    assert!(warning.is_none(), "a first run must produce no warning");
    assert_eq!(loaded, AppSettings::default());

    let _ = std::fs::remove_dir_all(&dir);
}
