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
    "unreleased-v1/references.namirpreset",
];

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn read_corpus_file(relative: &str) -> Vec<u8> {
    let path = corpus_dir().join(relative);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// FR-STATE-020's core claim: every document in the corpus restores successfully.
// trace: FR-STATE-020
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

/// Section 7 of `docs/04-state-and-preset-format.md`, read from hand-authored bytes rather than
/// from anything this crate's own writer produced: both reference slots, every documented field
/// of each, and FR-STATE-080's `embedded` object. Until this fixture existed, `references` was
/// exercised only by writer-to-reader round trips inside this crate — the exact weakness this
/// file's module doc names as its own reason to exist, since a reader that is accidentally
/// *stricter* than the documented format still agrees with the writer that shares its
/// assumptions.
///
/// `nam` carries §9's worked example verbatim (its `embedded.data`, its `display_name`, and a
/// foreign-platform `absolute`); `ir` is the same shape with no `embedded`, so the optional field
/// is exercised present *and* absent from on-disk bytes. `nam`'s `hash` is the real BLAKE3 hash of
/// the embedded payload, which is what makes P7's "identity is the content hash" checkable here
/// rather than merely a well-formed hex string.
#[test]
fn references_restore_every_documented_field_of_both_slots() {
    let bytes = read_corpus_file("unreleased-v1/references.namirpreset");
    let (state, warnings) = namir_state::State::read(&bytes).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");

    let nam = state.nam.expect("references.nam is present in the fixture");
    assert_eq!(
        nam.hash.to_string(),
        "dc57749e025523f24f989853b68405829607c4c84942579df0c3368694a531e3"
    );
    assert_eq!(
        nam.library_relative.as_ref().map(|p| p.as_str()),
        Some("marshall/plexi.nam")
    );
    // §7.1: `absolute` is verbatim and opaque -- a Windows-authored path is carried through this
    // Linux/macOS-or-Windows reader unparsed, backslashes and drive letter intact.
    assert_eq!(
        nam.absolute.as_deref(),
        Some("C:\\Users\\erwan\\Models\\plexi.nam")
    );
    assert_eq!(nam.display_name, "plexi.nam");
    let embedded = nam.embedded.expect("the nam slot carries an embedded copy");
    assert_eq!(embedded.media_type, "application/vnd.namir.nam+json");
    assert_eq!(
        embedded.data,
        br#"{"fake":"minimal nam-shaped json for corpus seeding"}"#
    );
    // P7: the recorded identity really is the content hash of the bytes carried alongside it.
    assert_eq!(nam.hash, namir_core::ContentHash::of(&embedded.data));

    let ir = state.ir.expect("references.ir is present in the fixture");
    assert_eq!(
        ir.hash.to_string(),
        "175b38765489b554a27a588061510a764a62f844dae9dfca6710eeda59055d13"
    );
    assert_eq!(
        ir.library_relative.as_ref().map(|p| p.as_str()),
        Some("cabs/1960a.wav")
    );
    assert_eq!(
        ir.absolute.as_deref(),
        Some("/home/erwan/irs/cabs/1960a.wav")
    );
    assert_eq!(ir.display_name, "1960a.wav");
    assert!(
        ir.embedded.is_none(),
        "`embedded` is optional -- the ir slot must load without one"
    );
}

/// The other direction of the same claim: what this build *writes* for a reference is
/// byte-for-byte the shape the hand-authored fixture holds. A reader-only tolerance (accepting a
/// field the writer never emits, or emitting one §7 does not document) would pass
/// [`references_restore_every_documented_field_of_both_slots`] and fail here.
#[test]
fn writing_the_references_fixture_back_reproduces_its_documented_section() {
    let bytes = read_corpus_file("unreleased-v1/references.namirpreset");
    let (state, _warnings) = namir_state::State::read(&bytes).unwrap();

    let on_disk: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let written: serde_json::Value = serde_json::from_slice(&state.write()).unwrap();
    assert_eq!(written["references"], on_disk["references"]);
}

/// Issue #113 / §7.4's four-step resolution order, driven end to end against the hand-authored
/// fixture rather than against a `FileRef` a test built in memory. The fixture's `nam` slot
/// carries all three external hints *and* an embedded copy; resolved on a machine that has none
/// of the three (no such library root, no such absolute path, nothing in the index — which is
/// precisely §7.2's "a preset shared with someone whose library is configured differently, or no
/// library at all"), the embedded copy is what makes it resolvable. Its `ir` slot, identical but
/// for having no embed, is the control: it reports missing, with the name and hash FR-STATE-070
/// says the user must be shown.
#[test]
fn the_embedded_copy_resolves_a_reference_no_configured_path_can_find() {
    let bytes = read_corpus_file("unreleased-v1/references.namirpreset");
    let (state, _warnings) = namir_state::State::read(&bytes).unwrap();

    let nam = state.nam.expect("references.nam is present in the fixture");
    match namir_state::resolve(&nam, &FindsNothing) {
        namir_state::Resolution::Embedded(embedded) => {
            // P7: what the fallback hands back really is the bytes the reference identifies.
            assert_eq!(namir_core::ContentHash::of(&embedded.data), nam.hash);
        }
        other => panic!("expected the embedded fallback, got {other:?}"),
    }

    let ir = state.ir.expect("references.ir is present in the fixture");
    match namir_state::resolve(&ir, &FindsNothing) {
        namir_state::Resolution::Missing(missing) => {
            assert_eq!(missing.display_name, "1960a.wav");
            assert_eq!(missing.hash, ir.hash);
        }
        other => panic!("the ir slot carries no embed and must be missing, got {other:?}"),
    }
}

/// A resolver on a machine that has none of the fixture's files — the UC-3 recipient.
struct FindsNothing;

impl namir_state::FileResolver for FindsNothing {
    fn resolve_library_relative(&self, _rel: &namir_state::RelPath) -> Option<PathBuf> {
        None
    }
    fn resolve_absolute(&self, _absolute: &str) -> Option<PathBuf> {
        None
    }
    fn resolve_by_hash(&self, _hash: namir_core::ContentHash) -> Option<PathBuf> {
        None
    }
}

/// Issue #112 against hand-authored bytes: unloading the model and saving over the document it
/// came from must actually remove `references.nam`. §7's "absent means nothing of that kind is
/// loaded" is the only way this format can express an empty slot, so a save that cannot write it
/// cannot express the user's own gesture — the model comes back on the next load.
#[test]
fn unloading_a_reference_and_saving_over_the_fixture_removes_its_slot() {
    let bytes = read_corpus_file("unreleased-v1/references.namirpreset");
    let original = namir_state::Document::parse(&bytes).unwrap();
    let (mut state, _warnings) = namir_state::State::read(&bytes).unwrap();
    assert!(state.nam.is_some() && state.ir.is_some());

    state.nam = None; // the user unloads the model
    let saved = state.write_onto(&original).to_pretty_bytes();

    let saved_json: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    assert!(
        saved_json["references"].get("nam").is_none(),
        "the cleared slot must be gone from the written bytes: {}",
        saved_json["references"]
    );
    let (reloaded, warnings) = namir_state::State::read(&saved).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(reloaded.nam, None);
    assert_eq!(reloaded.ir, state.ir, "the untouched slot survives intact");
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
