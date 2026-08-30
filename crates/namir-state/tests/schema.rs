//! FR-STATE-040's `S` half, asserted where it can be asserted against real documents: the
//! checked-in corpus, everything this build's writer produces, and the format document's own field
//! tables.
//!
//! `namir_state::schema`'s own unit tests cover each clause of §§3–7 of
//! `docs/04-state-and-preset-format.md` one at a time, against fixtures written to break exactly
//! one of them. This file is the other half of the evidence, and the one that could fail without
//! anyone editing the validator: it runs the same check over documents nobody wrote for it — the
//! six hand-authored corpus files, and the bytes `State::write`/`State::write_onto` actually
//! produce for a range of states.
//!
//! # Why a test here reads a document in `docs/`
//!
//! [`the_file_reference_table_of_section_7_1_names_exactly_the_fields_the_validator_checks`] and
//! its `embedded` twin parse `docs/04-state-and-preset-format.md` itself. That is deliberate and
//! is the same shape as `corpus.rs` reading `tests/corpus/`: the artifact under test is a
//! checked-in file, and the thing worth failing on is the two drifting apart. A schema check
//! transcribed from prose is only as good as the transcription, and these two tests are what make
//! "someone adds a sixth field to §7.1" a red test rather than a clause silently unchecked. It
//! costs this crate no dependency and no non-dev code — the path is resolved from
//! `CARGO_MANIFEST_DIR`, and nothing in `src/` reads a file at all (D-5.1's "never touches a
//! filesystem" is a statement about the crate's own code, which this does not change).

use std::path::{Path, PathBuf};

use namir_core::ContentHash;
use namir_state::{
    Document, EMBEDDED_FIELDS, FILE_REFERENCE_FIELDS, FileRef, RelPath, SchemaViolation, State,
};

/// The same list `corpus.rs` keeps, and kept separately on purpose: if a corpus file is added
/// there and not here, `every_corpus_document_is_checked` fails rather than this file quietly
/// checking a smaller set than the one the crate promises to keep loadable.
const CORPUS: &[&str] = &[
    "unreleased-v1/full.namirpreset",
    "unreleased-v1/minimal.namirpreset",
    "unreleased-v1/unknown-fields.namirpreset",
    "unreleased-v1/future-version.namirpreset",
    "unreleased-v1/legacy-global-section.namirpreset",
    "unreleased-v1/references.namirpreset",
];

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn read_corpus_file(relative: &str) -> Vec<u8> {
    let path = crate_dir().join("tests/corpus").join(relative);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn format_document() -> String {
    let path = crate_dir().join("../../docs/04-state-and-preset-format.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn describe(violations: &[SchemaViolation]) -> String {
    violations
        .iter()
        .map(|v| format!("\n  {v}"))
        .collect::<String>()
}

fn a_reference(name: &str, relative: &str, embedded: bool) -> FileRef {
    FileRef {
        hash: ContentHash::of(name.as_bytes()),
        library_relative: Some(RelPath::parse(relative).expect("well-formed")),
        absolute: Some(format!("C:\\Users\\erwan\\{name}")),
        display_name: name.to_string(),
        embedded: embedded.then(|| namir_state::EmbeddedRef {
            media_type: "application/vnd.namir.nam+json".to_string(),
            data: br#"{"fake":"bytes"}"#.to_vec(),
        }),
    }
}

/// Every state shape this build can save, as documents. Not a sample: the four axes are the four
/// things §§3–7 says a document may or may not carry — parameters at defaults or moved, each
/// reference slot filled or empty, an embedded copy present or not, and a legacy `global` section
/// underneath (which `write_onto` preserves per §5 and §8, so the saved document still has to
/// conform with one in it).
fn documents_this_build_writes() -> Vec<(String, Document)> {
    let mut out = Vec::new();

    out.push(("defaults".to_string(), State::defaults().into_document()));

    let mut moved = State::defaults();
    for descriptor in namir_params::REGISTRY {
        // Every parameter set away from its default at once, so no key of `parameters` is missing
        // from the document under test: §6's rules are per-key and this is the shape that puts
        // every key in front of them.
        // `set` clamps into the descriptor's own range (and to the last index of a stepped
        // parameter), so one out-of-range value moves every kind of parameter off its default
        // without this test needing to know any parameter's range.
        moved
            .params
            .set(descriptor.key, 1.0e9)
            .expect("REGISTRY's own key is a real parameter key");
    }
    out.push(("every parameter moved".to_string(), moved.into_document()));

    for (label, nam, ir) in [
        ("nam only", true, false),
        ("ir only", false, true),
        ("both references", true, true),
    ] {
        for embedded in [false, true] {
            let mut state = State::defaults();
            if nam {
                state.nam = Some(a_reference("plexi.nam", "marshall/plexi.nam", embedded));
            }
            if ir {
                state.ir = Some(a_reference("1960a.wav", "cabs/1960a.wav", embedded));
            }
            out.push((
                format!("{label}, embedded: {embedded}"),
                state.into_document(),
            ));
        }
    }

    // A save *onto* a pre-M6 document, which keeps the legacy `global` section §5 documents and
    // §8 promises to preserve. The one saved shape that carries a section a current writer never
    // emits, and therefore the one a writer-only conformance test would never look at.
    let legacy = Document::parse(&read_corpus_file(
        "unreleased-v1/legacy-global-section.namirpreset",
    ))
    .expect("a corpus document parses");
    let mut state = State::defaults();
    state.set_output_ceiling_db(-2.0);
    out.push((
        "saved onto a legacy `global` document".to_string(),
        state.write_onto(&legacy),
    ));

    // And a save onto the unknown-fields document, so §8's preserved top-level section is present
    // in a document under test: §3 says other top-level keys are legal, and a validator that had
    // quietly started reporting them would fail here.
    let unknown = Document::parse(&read_corpus_file(
        "unreleased-v1/unknown-fields.namirpreset",
    ))
    .expect("a corpus document parses");
    out.push((
        "saved onto a document with an unknown top-level section".to_string(),
        State::defaults().write_onto(&unknown),
    ));

    out
}

/// FR-STATE-040's `*Verify:*` line is `M plus S (schema check)`, and this is the `S`: every
/// document this build writes, and every hand-authored document it promises to read, conforms to
/// the format `docs/04-state-and-preset-format.md` §§3–7 documents — checked by
/// `namir_state::validate`, which restates those sections independently of the reader rather than
/// calling it (see `src/schema.rs`'s header). The `M` half is
/// `docs/manual-tests/fr-state-040-diffability-and-hand-editability.md`, which is what D-18.6
/// makes the traced artifact for that code and is unaffected by this file.
// trace: FR-STATE-040
#[test]
fn every_document_this_build_writes_conforms_to_the_documented_format() {
    for (label, document) in documents_this_build_writes() {
        let violations = namir_state::validate(&document);
        assert!(
            violations.is_empty(),
            "{label}: the document this build writes does not conform to \
             docs/04-state-and-preset-format.md §§3–7:{}\n{}",
            describe(&violations),
            String::from_utf8_lossy(&document.to_pretty_bytes())
        );
    }
}

/// The other direction, and the stronger claim of the two: bytes nobody generated from this
/// crate's own writer. A round-trip conformance test alone cannot catch a validator that has
/// drifted into agreeing with the writer, because the two would simply agree with each other —
/// which is `corpus.rs`'s own argument for keeping a hand-authored corpus at all.
// trace: FR-STATE-040
#[test]
fn every_corpus_document_conforms_to_the_documented_format() {
    for relative in CORPUS {
        let bytes = read_corpus_file(relative);
        let violations = namir_state::validate_bytes(&bytes)
            .unwrap_or_else(|e| panic!("{relative}: does not parse at all: {e}"));
        assert!(
            violations.is_empty(),
            "{relative}: does not conform to docs/04-state-and-preset-format.md §§3–7:{}",
            describe(&violations)
        );
    }
}

/// Without this, a corpus file added to `corpus.rs`'s manifest and not to this file's would be
/// schema-checked by nothing, and both files would stay green — the same failure
/// `the_corpus_directory_contains_exactly_the_manifest_no_more_no_less` exists to prevent one
/// level down.
#[test]
fn every_corpus_document_is_checked() {
    let dir = crate_dir().join("tests/corpus/unreleased-v1");
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .map(|e| format!("unreleased-v1/{}", e.file_name().to_string_lossy()))
        .collect();
    found.sort();
    let mut listed: Vec<String> = CORPUS.iter().map(|s| (*s).to_string()).collect();
    listed.sort();
    assert_eq!(found, listed);
}

/// Reads a `| Field | Required | ... |` table out of the format document and returns
/// `(field, required)` in the table's own order. The tables are the format document's own
/// statement of a shape, so parsing them is reading the specification, not guessing at it.
fn field_table(section_heading: &str) -> Vec<(String, bool)> {
    let text = format_document();
    let start = text.find(section_heading).unwrap_or_else(|| {
        panic!("{section_heading} is no longer a heading in the format document")
    });
    let rest = &text[start + section_heading.len()..];
    let end = rest.find("\n### ").unwrap_or(rest.len());

    rest[..end]
        .lines()
        .filter(|line| line.starts_with('|'))
        .filter_map(|line| {
            let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
            let field = cells.first()?.trim_matches('`');
            // `starts_with`, not equality: §7.1 writes `display_name`'s cell as
            // "no (empty string if absent)" and `hash`'s as "**yes**". The header row's
            // "Required" and the `|---|` separator match neither and are skipped.
            let required = cells.get(1)?.trim_matches('*');
            if required.starts_with("yes") {
                Some((field.to_string(), true))
            } else if required.starts_with("no") {
                Some((field.to_string(), false))
            } else {
                None
            }
        })
        .collect()
}

/// §7.1's table against [`FILE_REFERENCE_FIELDS`]. What this catches is the residue a
/// prose-transcribed schema check has and a generated one would not: a field added to the format
/// document, or a `Required` flag changed there, with the validator left as it was.
// trace: FR-STATE-040
#[test]
fn the_file_reference_table_of_section_7_1_names_exactly_the_fields_the_validator_checks() {
    let documented = field_table("### 7.1 File reference shape");
    let checked: Vec<(String, bool)> = FILE_REFERENCE_FIELDS
        .iter()
        .map(|(name, required)| ((*name).to_string(), *required))
        .collect();
    assert_eq!(documented, checked);
}

/// §7.2's table against [`EMBEDDED_FIELDS`]. See the test above.
// trace: FR-STATE-040
#[test]
fn the_embedded_table_of_section_7_2_names_exactly_the_fields_the_validator_checks() {
    let documented = field_table("### 7.2 `embedded` (FR-STATE-080)");
    let checked: Vec<(String, bool)> = EMBEDDED_FIELDS
        .iter()
        .map(|(name, required)| ((*name).to_string(), *required))
        .collect();
    assert_eq!(documented, checked);
}

/// The check has to be able to fail, and on a document of exactly the kind FR-STATE-040 exists to
/// make possible: one a human hand-edited. Four independent mistakes, four reported clauses, and
/// the document still *loads* — which is the point of the severity distinction. Tolerant loading
/// is silent by design, so without a schema check none of these four would ever be told to anyone.
#[test]
fn a_hand_edited_document_reports_every_clause_it_breaks_and_still_loads() {
    let hand_edited = br#"{
        "format_version": 1,
        "parameters": { "trim.gain_db": "3 dB please" },
        "references": {
            "nam": {
                "hash": "not-a-hash",
                "library_relative": "../escape/plexi.nam",
                "embedded": { "encoding": "gzip", "data": "AAAA" }
            }
        }
    }"#;

    let violations = namir_state::validate_bytes(hand_edited).expect("it is still valid JSON");
    let pointers: Vec<&str> = violations.iter().map(|v| v.pointer.as_str()).collect();
    assert_eq!(
        pointers,
        [
            "/parameters/trim.gain_db",
            "/references/nam/hash",
            "/references/nam/library_relative",
            "/references/nam/embedded/encoding",
        ],
        "{}",
        describe(&violations)
    );

    // Every one of them is `Recovered`, and the document really does load: D-11.2's tolerance is
    // what makes a schema check worth having rather than redundant with the reader.
    assert!(
        violations
            .iter()
            .all(|v| v.severity == namir_state::Severity::Recovered)
    );
    assert!(State::read(hand_edited).is_ok());
}
