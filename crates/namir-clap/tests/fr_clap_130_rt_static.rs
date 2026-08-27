//! **FR-CLAP-130's `S` half** (Must, `Verify:` **S plus I**): "The plugin shall never block the
//! audio thread waiting on the GUI thread, the file system, or the host, under any user action
//! including model recall, preset recall and library scanning."
//!
//! The `I` half is `tests/clap_host_rt_blocking.rs` — a live instance processing on its own thread
//! while a recall, a model load and a scan-shaped worker job contend underneath it, with the bound
//! derived from a stall that run measures for itself. It has existed since M9b. **The `S` half had
//! no artifact at all**, which is what FR-CLAP-130's `uncovered:` field said, and is what this file
//! is.
//!
//! # Why a static check is needed *beside* the dynamic one, not instead of it
//!
//! `clap_host_rt_blocking.rs` can only observe the contention it manages to create. A lock taken on
//! a path that run never enters — a rare host callback, an error branch, a future edit — is
//! invisible to it, and would be a live NFR-RT-010 violation that the green test says nothing
//! about. What a source-level ban catches is the *introduction* of the mechanism, on any path,
//! including one no test reaches yet.
//!
//! This is `xtask rt-logging`'s device (FR-ERR-030's own `S` half), applied to a different name
//! list. That check lives in `xtask` and is a workspace-wide gate; this one lives here because
//! [`FORBIDDEN`] below is a statement about *this crate's two audio-thread entry points* and about
//! `SharedInner`'s own `pub(crate)` API, neither of which `xtask` can see. If FR-CLAP-130 ever
//! wants the same treatment for `namir-app`'s `cpal` callback, that is `xtask`'s to own.
//!
//! # What is scanned, and at what granularity
//!
//! Two regions, both read with [`include_str!`] so a rename or a move is a **compile** error rather
//! than a silently-skipped check — the same failure mode `xtask rt-logging` guards with its
//! unreadable-file arm.
//!
//! - **`src/audio.rs`, whole file.** It mixes threads: `process`, `reset`, `process_segment`,
//!   `apply_direct_and_mirror`, `publish_latency`, `process_port_pair` and `prepare_channel` are
//!   audio-thread, while `activate`/`deactivate` are CLAP `[main-thread]`. The ban is applied to
//!   the whole file anyway, because [`FORBIDDEN`] holds no name the main-thread half legitimately
//!   uses — `activate` reaches `SharedInner` through `install_instance`,
//!   `set_telemetry_reader` and `push_notice`, none of which is on the list. That is an
//!   over-approximation, and the direction is the safe one: it can raise a false alarm on a
//!   main-thread function and can never let an audio-thread call through.
//! - **`src/params_ext.rs`, the `PluginAudioProcessorParams` impl block only.** Whole-file scoping
//!   is not available here and the reason is instructive rather than incidental: the *main-thread*
//!   `PluginMainThreadParams::flush` in the same file calls `with_instance` and
//!   `try_submit_param`, which are exactly two of the names this check exists to forbid on the
//!   audio thread. The two `flush` implementations sitting next to each other, one allowed to lock
//!   and one not, is the single most likely place in this crate for the wrong one to be edited.
//!
//! # Residual blind spots, stated rather than pretended closed
//!
//! 1. **Not transitive.** This forbids *naming* a blocking primitive in an audio-thread region, not
//!    *reaching* one. A helper defined elsewhere that locks internally can still be called. The
//!    mitigation is that this crate's audio thread reaches `SharedInner` and `Instance` only
//!    through the names on the list, so the transitive step has to go through one of them.
//! 2. **The host handle is not banned outright.** `publish_latency` calls
//!    `host.shared().request_callback()`, which `clack_extensions::latency` documents as
//!    `[thread-safe]` and which returns without waiting — a host *call* is not a host *wait*, and
//!    the requirement forbids the second. A blocking host round trip would have to arrive through a
//!    method this list does not know about; only review covers that.
//! 3. **Whole-identifier matching, not a parse.** A name reached through an alias
//!    (`use std::sync::Mutex as M;`) evades the list. The `use` itself would not, and in Rust it is
//!    necessarily in the same file — the same argument `xtask rt-logging` makes for banning the
//!    import rather than the call.

use std::collections::BTreeSet;

/// `crates/namir-clap/src/audio.rs`, read at compile time.
const AUDIO_RS: &str = include_str!("../src/audio.rs");

/// `crates/namir-clap/src/params_ext.rs`, read at compile time.
const PARAMS_EXT_RS: &str = include_str!("../src/params_ext.rs");

/// The line that opens the audio-thread half of `params_ext.rs`. Matched by `trim_start`ed prefix,
/// so a `where` clause or a lifetime edit does not silently un-cover the block — but a rename of
/// the trait does, which is why [`the_audio_thread_flush_region_is_actually_found`] asserts the
/// region was located and is not empty.
const AUDIO_FLUSH_IMPL: &str = "impl<'a> PluginAudioProcessorParams for NamirAudioProcessor<'a>";

/// Identifiers no audio-thread region in this crate may name.
///
/// Three groups, and every entry is a name the audio thread would have to *wait* on:
///
/// - **Blocking primitives.** `lock` is the method every `Mutex`/`RwLock` acquisition goes through
///   whatever the type is spelled as; `join`, `park`, `recv` and `sleep` are the other ways a
///   thread stops.
/// - **The file system.** `fs`, `File` and the `read_to_*` family cover `std::fs::read`,
///   `File::open` and every path through them. **The network is deliberately not on this list**,
///   even though "waiting on the file system" and "waiting on a socket" are the same failure for
///   this requirement: `xtask network-free` (FR-ERR-060/NFR-SEC-030) already bans every `std::net`
///   name in every first-party crate, workspace-wide — which is stronger than anything scoped to
///   two modules, and which includes this file's own source. Restating two of its names here was
///   tried, and it failed that gate; correctly, since a name list is a name list wherever it is
///   written.
/// - **`SharedInner`'s own locking API.** These are the crate-internal names through which an
///   audio-thread edit would most plausibly acquire the instance mutex — `with_instance` is the
///   accessor `src/audio.rs`'s module doc comment says `process()` must never take, and
///   `try_submit_param` is the ring-side path whose producer mutex a worker job can hold for
///   `CommandSubmitter::DEFAULT_DEADLINE` (2 seconds). `start_library_scan` is on the list because
///   it is the third of the requirement's three enumerated user actions.
const FORBIDDEN: &[&str] = &[
    // Blocking primitives.
    "Mutex",
    "RwLock",
    "Condvar",
    "Barrier",
    "MutexGuard",
    "lock",
    "join",
    "park",
    "recv",
    "sleep",
    // The file system.
    "fs",
    "File",
    "OpenOptions",
    "read_to_end",
    "read_to_string",
    // `SharedInner`'s locking API and the ring-side parameter path.
    "with_instance",
    "try_submit_param",
    "start_library_scan",
    "cancel_library_scan",
    "library_snapshot",
    "notices",
];

/// Every whole-identifier occurrence of `name` in `source`, ignoring `//`-comment text and doc
/// comments — this file's own prose, and `audio.rs`'s, names most of the forbidden list while
/// explaining why it is forbidden.
///
/// "Whole identifier" means the characters either side are not identifier characters, so `fs`
/// matches `std::fs::read` and `use std::fs;` but not `offset` or `fs_like`.
fn code_occurrences(source: &str, name: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let code = match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        };
        let bytes = code.as_bytes();
        let mut from = 0;
        while let Some(offset) = code[from..].find(name) {
            let start = from + offset;
            let end = start + name.len();
            let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
            let after_ok = end == bytes.len() || !is_ident_byte(bytes[end]);
            if before_ok && after_ok {
                hits.push(index + 1);
            }
            from = end;
        }
    }
    hits
}

/// Whether `b` can appear inside a Rust identifier.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The `impl` block opened by the line whose `trim_start`ed form begins with `marker`, up to and
/// including the first line that is exactly `}` — the closing brace of a top-level item under
/// rustfmt.
///
/// Returns `None` if the marker is absent, which every caller treats as a violation rather than as
/// "nothing to check".
fn top_level_block<'a>(source: &'a str, marker: &str) -> Option<&'a str> {
    let start = source
        .lines()
        .position(|line| line.trim_start().starts_with(marker))?;
    let end = source
        .lines()
        .skip(start + 1)
        .position(|line| line == "}")?;
    let region: Vec<&str> = source.lines().skip(start).take(end + 2).collect();
    // Rebuild by slicing the original rather than joining, so reported line numbers stay usable:
    // the caller only needs the text, and `code_occurrences` reports offsets within it.
    let first = source
        .lines()
        .take(start)
        .map(|l| l.len() + 1)
        .sum::<usize>();
    let last = first
        + region
            .iter()
            .map(|l| l.len() + 1)
            .sum::<usize>()
            .min(source.len() - first);
    Some(&source[first..last])
}

/// Asserts `region` (described by `what`) names none of [`FORBIDDEN`].
fn assert_no_forbidden_names(what: &str, region: &str) {
    let mut found: BTreeSet<String> = BTreeSet::new();
    for name in FORBIDDEN {
        for line in code_occurrences(region, name) {
            found.insert(format!("{name} (line {line} of the scanned region)"));
        }
    }
    assert!(
        found.is_empty(),
        "FR-CLAP-130/NFR-RT-010: {what} names {} forbidden identifier(s) on the audio thread. \
         Every entry below is something the audio thread would have to wait on -- a lock a non-RT \
         thread can hold, a file-system or network call, or one of `SharedInner`'s locking \
         accessors. If the new use is genuinely on the `[main-thread]` half of a mixed file, move \
         it to a module that is not scanned (the escape hatch `xtask rt-logging` documents for the \
         same situation); do not widen this list.\n  {}",
        found.len(),
        found.into_iter().collect::<Vec<_>>().join("\n  ")
    );
}

/// FR-CLAP-130's static limb: neither audio-thread region in this crate names a blocking
/// primitive, a file-system or network call, or one of `SharedInner`'s locking accessors.
// trace-partial: FR-CLAP-130
// uncovered: FR-CLAP-130 — of the three enumerated user actions only model loading and preset
// uncovered: recall are driven for real by the I half (clap_host_rt_blocking.rs), library scanning
// uncovered: standing in as a worker job that reads, hashes and parses a multi-megabyte file while
// uncovered: holding the same instance mutex a scan's jobs take, because starting a real scan
// uncovered: would erase the developer's own library index; and the S half this file adds is a
// uncovered: whole-identifier name ban rather than a call-graph analysis, so it catches the
// uncovered: introduction of a blocking primitive in an audio-thread region but not a helper
// uncovered: defined elsewhere that blocks internally; closes M8
#[test]
fn fr_clap_130_no_audio_thread_region_names_a_blocking_primitive() {
    assert_no_forbidden_names("crates/namir-clap/src/audio.rs", AUDIO_RS);

    let flush = top_level_block(PARAMS_EXT_RS, AUDIO_FLUSH_IMPL).unwrap_or_else(|| {
        panic!(
            "crates/namir-clap/src/params_ext.rs no longer contains a top-level block starting \
             {AUDIO_FLUSH_IMPL:?}. The audio-thread `flush` was renamed, moved or reformatted, and \
             this check has stopped covering it -- re-point `AUDIO_FLUSH_IMPL` rather than \
             deleting the assertion"
        )
    });
    assert_no_forbidden_names(
        "crates/namir-clap/src/params_ext.rs's PluginAudioProcessorParams::flush",
        flush,
    );
}

/// The scanner's own negative control. A check that silently matched nothing would pass this file's
/// main assertion for the wrong reason, and the region extractor is the part most likely to fail
/// that way.
#[test]
fn the_audio_thread_flush_region_is_actually_found() {
    let flush = top_level_block(PARAMS_EXT_RS, AUDIO_FLUSH_IMPL).expect("the region must be found");
    assert!(
        flush.contains("fn flush"),
        "the extracted region should contain the audio-thread flush itself"
    );
    assert!(
        flush.contains("apply_direct_and_mirror"),
        "the extracted region should contain the direct-apply call that makes it RT-safe"
    );
    assert!(
        !flush.contains("PluginMainThreadParams"),
        "the extracted region has run past the end of its own impl block and swallowed the \
         main-thread one, which would make the ban vacuous in the other direction"
    );

    // And the ban itself has teeth: the *main-thread* half of the same file, which is allowed to
    // lock, is caught by the same list. If this stops being true the list has been hollowed out.
    let mut caught = false;
    for name in FORBIDDEN {
        if !code_occurrences(PARAMS_EXT_RS, name).is_empty() {
            caught = true;
        }
    }
    assert!(
        caught,
        "params_ext.rs as a whole should still name at least one forbidden identifier (its \
         main-thread flush takes the instance mutex), or FORBIDDEN no longer describes anything"
    );
}

/// The positive half of the same statement: `audio.rs` does reach the engine, and does it through
/// the non-locking path the module's own doc comment argues for.
///
/// Without this, a future edit could satisfy the ban above by deleting the parameter path
/// altogether.
#[test]
fn the_audio_thread_still_applies_parameters_through_the_direct_path() {
    assert!(
        !code_occurrences(AUDIO_RS, "apply_param_direct").is_empty(),
        "src/audio.rs no longer calls `AudioEngine::apply_param_direct`; the two-delivery-path \
         split FR-CLAP-130 rests on has been dismantled"
    );
    assert!(
        !code_occurrences(AUDIO_RS, "reset_direct").is_empty(),
        "src/audio.rs no longer calls `AudioEngine::reset_direct`"
    );
}
