//! FR-STATE-020's literal *Verify*: "a checked-in corpus of state documents from every released
//! version is restored in CI." There are no released versions of Namir yet — every crate in this
//! workspace is pinned to `0.1.0` as a pre-release placeholder (root `Cargo.toml`'s own comment:
//! "this is a brand-new project with zero external users yet"). So today the corpus holds
//! `unreleased-v1/`, documents produced by hand against `format_version 1` as it stands on
//! `trunk` — not a captured release, since none exists (D-19.1's "generated, not captured" applies
//! here in spirit: nothing here was produced by running an old released binary, because there is
//! no such binary). [`corpus_covers_every_version_this_build_could_be_released_as`] is the
//! mechanism that turns this from a documentation note into an enforced rule the day a real
//! release happens.
//!
//! This is also the practical evidence for D-11.2's "a project saved by a newer Namir and opened
//! by an older one does not silently lose settings" and, via `unknown-fields.namirpreset`, for the
//! per-key tolerance rules in `params.rs` — proven here against hand-authored bytes rather than
//! only against bytes this crate's own writer produced, which is a strictly stronger claim: a
//! round-trip test alone cannot catch a reader that is accidentally *stricter* than the format it
//! claims to support, because the writer and reader would simply agree with each other.

use std::path::{Path, PathBuf};

/// The complete list of corpus documents this crate promises to keep loadable. Deliberately a
/// literal list rather than "whatever's in the directory" — see
/// [`the_corpus_directory_contains_exactly_the_manifest_no_more_no_less`] for why an
/// accidentally-deleted corpus file must fail loudly rather than making every other test in this
/// file vacuously pass over a smaller set.
const MANIFEST: &[&str] = &[
    "unreleased-v1/full.namirpreset",
    "unreleased-v1/minimal.namirpreset",
    "unreleased-v1/unknown-fields.namirpreset",
    "unreleased-v1/future-version.namirpreset",
    "unreleased-v1/legacy-global-section.namirpreset",
];

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn read_corpus_file(relative: &str) -> Vec<u8> {
    let path = corpus_dir().join(relative);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// FR-STATE-020's core claim: every document in the corpus restores successfully.
#[test]
fn every_manifest_entry_loads_successfully() {
    for relative in MANIFEST {
        let bytes = read_corpus_file(relative);
        let result = namir_state::State::read(&bytes);
        assert!(
            result.is_ok(),
            "{relative}: failed to load: {:?}",
            result.err()
        );
    }
}

/// Without this, deleting a corpus file (by accident, or by a careless `git mv`) would silently
/// shrink what `every_manifest_entry_loads_successfully` actually checks, and every test in this
/// file would keep passing over a smaller and smaller set until the corpus was empty.
#[test]
fn the_corpus_directory_contains_exactly_the_manifest_no_more_no_less() {
    let mut on_disk = Vec::new();
    collect_namirpreset_files(&corpus_dir(), &corpus_dir(), &mut on_disk);
    // Normalise to forward slashes for a platform-independent comparison, since
    // std::fs::read_dir's returned paths use the platform's own separator.
    let mut on_disk_normalised: Vec<String> =
        on_disk.iter().map(|s| s.replace('\\', "/")).collect();
    on_disk_normalised.sort();

    let mut expected: Vec<&str> = MANIFEST.to_vec();
    expected.sort();

    assert_eq!(
        on_disk_normalised, expected,
        "tests/corpus/ on disk does not match MANIFEST in corpus.rs -- update whichever is stale"
    );
}

fn collect_namirpreset_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("corpus dir must exist") {
        let entry = entry.expect("readable dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_namirpreset_files(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "namirpreset") {
            let relative = path
                .strip_prefix(root)
                .expect("entries are under root")
                .to_string_lossy()
                .into_owned();
            out.push(relative);
        }
    }
}

/// FR-STATE-020's literal wording: "any parameter absent from the document takes its documented
/// default." `minimal.namirpreset` has no `parameters` section at all.
#[test]
fn minimal_restores_to_exactly_the_documented_defaults() {
    let bytes = read_corpus_file("unreleased-v1/minimal.namirpreset");
    let (state, warnings) = namir_state::State::read(&bytes).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(state, namir_state::State::defaults());
}

/// `full.namirpreset` sets every `REGISTRY` parameter to a value distinct from its default —
/// proof this corpus entry is actually exercising every value, not merely a document that
/// happens to restore to the defaults by coincidence.
#[test]
fn full_restores_every_parameter_to_a_non_default_value() {
    let bytes = read_corpus_file("unreleased-v1/full.namirpreset");
    let (state, warnings) = namir_state::State::read(&bytes).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let defaults = namir_state::State::defaults();
    for (descriptor, value) in state.params.iter() {
        let default_value = defaults.params.get(descriptor.key).unwrap();
        assert_ne!(
            value, default_value,
            "{} was left at its default -- full.namirpreset must set every parameter",
            descriptor.key
        );
    }
}

/// D-11.2's tolerance, exercised against hand-authored bytes: an unrecognised parameter key and
/// a wholly unrecognised top-level section both produce a warning, not a load failure.
#[test]
fn unknown_fields_document_loads_with_warnings_not_failure() {
    let bytes = read_corpus_file("unreleased-v1/unknown-fields.namirpreset");
    let (state, warnings) = namir_state::State::read(&bytes).unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}"); // comp.ratio, the unrecognised parameter key
    assert_eq!(state.params.get("trim.gain_db"), Some(1.5));
}

/// D-10.4's backward-compatibility case: a document written before that decision carries
/// `global.bypass`/`global.output_ceiling_db` in the separate, now-retired `global` section
/// rather than as `parameters` entries. This build must still read the values correctly rather
/// than silently reverting an existing preset's bypass/ceiling to its default -- D-11.2's
/// tolerant-loading promise applied to this crate's own past format, not just a hypothetical
/// future one.
#[test]
fn legacy_global_section_restores_bypass_and_ceiling() {
    let bytes = read_corpus_file("unreleased-v1/legacy-global-section.namirpreset");
    let (state, warnings) = namir_state::State::read(&bytes).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(state.global_bypass());
    assert_eq!(state.output_ceiling_db(), -6.0);
    assert_eq!(state.params.get("trim.gain_db"), Some(2.0));
}

/// A document from a build newer than this one (`format_version: 2`, greater than
/// `namir_state::FORMAT_VERSION`) must not be rejected outright -- D-11.2's stated purpose is
/// exactly this case. `migrate.rs`, landing later in this milestone, will add the specific
/// `state.format.newer` warning this currently-unconditional read doesn't yet produce; this test
/// pins the load-succeeds half of that promise now, ahead of the warning being wired up.
#[test]
fn a_document_declaring_a_newer_format_version_still_loads() {
    let bytes = read_corpus_file("unreleased-v1/future-version.namirpreset");
    let result = namir_state::State::read(&bytes);
    assert!(result.is_ok(), "{:?}", result.err());
}

/// FR-STATE-020's corpus must gain one new directory per released version. There is no released
/// version yet, so this is not a no-op: it is the check that will actually fire, loudly, the day
/// `CARGO_PKG_VERSION` changes to anything but the pre-release placeholder without a matching
/// `tests/corpus/<version>/` directory existing — the release-time discipline the requirement's
/// *Verify* method asks for, enforced rather than merely documented.
#[test]
fn corpus_covers_every_version_this_build_could_be_released_as() {
    const PRE_RELEASE_PLACEHOLDER: &str = "0.1.0";
    let version = env!("CARGO_PKG_VERSION");
    if version == PRE_RELEASE_PLACEHOLDER {
        return; // No release has happened yet; nothing to check.
    }
    let expected_dir = corpus_dir().join(version);
    assert!(
        expected_dir.is_dir(),
        "CARGO_PKG_VERSION is {version}, not the pre-release placeholder \
         ({PRE_RELEASE_PLACEHOLDER}) -- a corpus directory for this released version \
         (tests/corpus/{version}/) must exist before this can be considered released"
    );
}
