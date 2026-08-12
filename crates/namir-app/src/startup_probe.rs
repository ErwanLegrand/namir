//! NFR-PERF-030's measurement seam, and nothing else: "the standalone application shall reach an
//! audible state (audio streaming, default state loaded) within 3 seconds on the reference machine
//! with a warm library index." `*Verify:* B`.
//!
//! # Why a seam exists at all
//!
//! The requirement is about the *process*, from launch to audio flowing, so the only honest way to
//! time it is to launch the real `namir` binary and watch for the moment it becomes audible. Two
//! things stand in the way of doing that from a benchmark, and this module is exactly the two
//! answers:
//!
//! - **The audible instant is unobservable from outside.** [`crate::stream::RunningStreams::play`]
//!   returning `Ok(())` is, in that method's own words, "the one call that actually makes audio
//!   flow"; before this module the only trace it left was an `eprintln!` sharing a stream with
//!   every other start-up log line and carrying no measurable content. [`audible`] emits a single
//!   machine-readable marker on **stdout** at that instant instead.
//! - **The process never exits.** [`crate::app::run`] blocks in `namir_ui::open_blocking` until the
//!   window is closed, so a harness that spawned it would measure a human. Under the probe,
//!   `run` returns immediately after the marker.
//!
//! # The shape of the seam, and why it is this shape
//!
//! One environment variable, [`PROBE_ENV`], whose **value is the configuration directory** the
//! probed launch uses in place of [`namir_platform::config_dir`]. Presence turns the probe on;
//! absence leaves every path in this crate exactly as it was, which is the property that matters
//! most — an ordinary launch never reads this variable's value for anything and never takes a
//! different branch because of it.
//!
//! Carrying the directory in the same variable rather than adding a second one is deliberate on
//! two counts. It makes the probe's measured launch *reproducible*: the requirement's "with a warm
//! library index" precondition is then a condition the harness establishes (an index file it wrote,
//! at a size it chose) rather than whatever the machine's real library happens to contain. And it
//! means a measurement cannot write to the real per-user configuration directory — the probe
//! returns before `crate::app::run`'s `settings::save`, but pointing it elsewhere makes that a
//! property of the seam rather than of one `return` statement's position.
//!
//! # The marker grammar
//!
//! One line, on stdout, flushed:
//!
//! ```text
//! namir-startup-probe: audible in_process_ms=412.907 library_index_entries=10000 default_state_params=27
//! namir-startup-probe: not-audible reason=stream-not-started in_process_ms=88.114 detail=the audio backend refused the requested format
//! ```
//!
//! Every field is a whitespace-delimited `key=value` pair except `detail`, which is always **last**
//! and runs to the end of the line, because it carries an error message with spaces in it. [`field`]
//! reads the former, [`detail`] the latter.
//!
//! The fields are the seam's whole reason for reporting anything beyond the instant itself:
//!
//! - `library_index_entries` — how many entries were in the index this launch read. Lets the
//!   harness *check* the "warm library index" precondition instead of assuming it.
//! - `default_state_params` — how many parameters [`namir_state::State::defaults`] carried when it
//!   was built. The requirement's other half, "default state loaded", has no event of its own:
//!   nothing in `crate::app::run` announces it, and it is satisfied implicitly by
//!   `build_default_engine` and `State::defaults` both running well before any stream opens. Rather
//!   than invent an event and then assert the invention, the marker reports what was actually
//!   built, and the benchmark checks it against `namir_params::REGISTRY`'s own length — so
//!   "default state loaded" is a checked precondition of the timing, not a claim about source
//!   ordering.
//! - `in_process_ms` — measured from [`entered`], the first statement of `crate::app::run`.
//!   **Diagnostic only.** The figure NFR-PERF-030 is asserted against is the harness's own
//!   wall-clock from before `Command::spawn`, which this cannot see: process creation, image load
//!   and dynamic linking all happen before `run` starts, and a user waits for those too. Its use is
//!   attribution — a measurement over budget is a different problem depending on which side of this
//!   number the time went.
//! - `detail` — why a not-audible launch was not audible, when the reason token alone is not
//!   enough. It is not decoration: for [`REASON_STREAM_NOT_STARTED`] the underlying
//!   `AudioIoError` goes to `AppHost::report`, i.e. into a UI notice, and a probed launch opens no
//!   window to show it — measured, not assumed, by holding the device open with a second `namir`
//!   and watching the harness receive that reason with an entirely empty stderr behind it.
//!   [`REASON_NO_AUDIO_DEVICE`] carries no `detail` because each of its four call sites has already
//!   printed its own explanation on stderr, which a harness capturing the process already has.
//!
//! [`field`] is the reading half of that grammar, so the benchmark parses what this module writes
//! rather than a second copy of the format that can drift from it.
//!
//! # What this module deliberately does not do
//!
//! It does not touch the audio thread. A stronger definition of "audible" is available — waiting
//! for the output callback to have processed its first block, which would prove audio genuinely
//! flowed rather than that the streams were told to start — and it is rejected here: it would put
//! an observable inside `crate::stream`'s callback, on the single most-reviewed path in this
//! project, purely to enable a measurement. The residue that leaves is real and is recorded, not
//! glossed: see the `// uncovered:` field on `benches/startup_to_audible.rs`'s `main`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

/// The environment variable that turns the probe on, and whose value is the configuration
/// directory the probed launch uses. Unset (or empty) means every path in this crate behaves
/// exactly as it does in an ordinary launch.
pub const PROBE_ENV: &str = "NAMIR_STARTUP_PROBE";

/// The stdout marker [`audible`] emits, at the instant
/// [`crate::stream::RunningStreams::play`] returns `Ok(())`.
pub const AUDIBLE_MARKER: &str = "namir-startup-probe: audible";

/// The stdout marker [`not_audible`] emits when a launch settled without audio — a distinct
/// outcome from "took too long", and one a harness must report separately rather than count as a
/// timeout.
pub const NOT_AUDIBLE_MARKER: &str = "namir-startup-probe: not-audible";

/// [`not_audible`]'s reason token for a launch that never got as far as opening a stream at all:
/// no usable device, an unusable negotiated sample rate, or an engine that would not prepare or
/// build. All four divert to `crate::app::open_window_without_audio`, and each has already printed
/// its own explanation on stderr — which the harness captures — so one token is enough here.
pub const REASON_NO_AUDIO_DEVICE: &str = "no-audio-device";

/// [`not_audible`]'s reason token for the softer failure: devices were found and the engine was
/// wired, but `crate::stream::open` or `RunningStreams::play` failed. The window would still open
/// in an ordinary launch, with no audio behind it.
pub const REASON_STREAM_NOT_STARTED: &str = "stream-not-started";

/// Set once by [`entered`]. A `OnceLock` rather than a plain `static mut` or a thread-local
/// because `crate::app::run` is called once per process by construction (`main.rs` is a single
/// call) and a second `set` must be a no-op rather than a moved origin.
static ENTERED: OnceLock<Instant> = OnceLock::new();

/// The configuration directory this launch was pointed at, or `None` for an ordinary launch.
/// `crate::app::resolve_config_dir` prefers this over [`namir_platform::config_dir`].
pub fn config_dir_override() -> Option<PathBuf> {
    let value = std::env::var_os(PROBE_ENV)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// Whether this launch is a measurement run.
pub fn enabled() -> bool {
    config_dir_override().is_some()
}

/// Records the in-process origin for `in_process_ms`. Called as the first statement of
/// `crate::app::run`, unconditionally: one `Instant::now()` and one uncontended `OnceLock::set`,
/// which is not a behavioural difference an ordinary launch can observe, and gating it on
/// [`enabled`] would put an environment lookup on the ordinary path instead.
pub fn entered() {
    let _ = ENTERED.set(Instant::now());
}

/// Emits the audible marker. **The marking event for NFR-PERF-030**: called at the instant
/// `RunningStreams::play` returns `Ok(())`, before anything else that arm does, so the interval a
/// harness measures ends where the requirement says it does rather than one log line later.
///
/// A no-op in an ordinary launch.
pub fn audible(library_index_entries: usize, default_state_params: usize) {
    if !enabled() {
        return;
    }
    emit(format!(
        "{AUDIBLE_MARKER} in_process_ms={:.3} library_index_entries={library_index_entries} \
         default_state_params={default_state_params}",
        in_process_ms()
    ));
}

/// Emits the not-audible marker with `reason` — one of [`REASON_NO_AUDIO_DEVICE`] or
/// [`REASON_STREAM_NOT_STARTED`] — and `detail`, free text that may contain spaces and is
/// therefore written last. Pass `""` where the reason token stands on its own. A no-op in an
/// ordinary launch.
pub fn not_audible(reason: &str, detail: &str) {
    if !enabled() {
        return;
    }
    let mut line = format!(
        "{NOT_AUDIBLE_MARKER} reason={reason} in_process_ms={:.3}",
        in_process_ms()
    );
    if !detail.is_empty() {
        // Newlines would split one marker into two lines, and a harness reads one.
        line.push_str(" detail=");
        line.push_str(&detail.replace(['\r', '\n'], " "));
    }
    emit(line);
}

/// The value of `key` in a marker line, or `None` if the line does not carry that field. The
/// reading half of the grammar this module's doc comment describes — public so the benchmark
/// parses what [`audible`]/[`not_audible`] actually write. Not for `detail`, which is free text
/// running to the end of the line; use [`detail`] for that.
pub fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
}

/// A marker line's trailing `detail=` text, or `None` if it carries none.
pub fn detail(line: &str) -> Option<&str> {
    line.split_once(" detail=").map(|(_, rest)| rest)
}

fn in_process_ms() -> f64 {
    ENTERED
        .get()
        .map(|origin| origin.elapsed().as_secs_f64() * 1000.0)
        .unwrap_or(f64::NAN)
}

/// Stdout, not stderr, and flushed: every other start-up line this crate prints goes to stderr, so
/// a harness reading stdout sees the marker and nothing else, and needs no filtering to find it.
fn emit(line: String) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grammar's two halves agree. Written against a literal line rather than against
    /// [`audible`]'s output because that function reads the process environment, which a test must
    /// not mutate (`std::env::set_var` is `unsafe` in this edition and process-global besides —
    /// the same reasoning `namir_platform::paths`' own tests give).
    #[test]
    fn field_reads_back_every_field_the_audible_marker_writes() {
        let line = format!(
            "{AUDIBLE_MARKER} in_process_ms=412.907 library_index_entries=10000 \
             default_state_params=27"
        );
        assert!(line.starts_with(AUDIBLE_MARKER));
        assert_eq!(field(&line, "in_process_ms"), Some("412.907"));
        assert_eq!(field(&line, "library_index_entries"), Some("10000"));
        assert_eq!(field(&line, "default_state_params"), Some("27"));
        assert_eq!(field(&line, "reason"), None);
    }

    #[test]
    fn field_reads_the_not_audible_markers_reason() {
        let line =
            format!("{NOT_AUDIBLE_MARKER} reason={REASON_NO_AUDIO_DEVICE} in_process_ms=88.1");
        assert!(line.starts_with(NOT_AUDIBLE_MARKER));
        assert_eq!(field(&line, "reason"), Some(REASON_NO_AUDIO_DEVICE));
        assert_eq!(detail(&line), None);
    }

    /// `detail` is last and runs to the end of the line, spaces and all — the reason it is not an
    /// ordinary whitespace-delimited field. The fields before it must still read back.
    #[test]
    fn detail_carries_a_message_with_spaces_and_leaves_the_earlier_fields_readable() {
        let line = format!(
            "{NOT_AUDIBLE_MARKER} reason={REASON_STREAM_NOT_STARTED} in_process_ms=88.1 \
             detail=the device is already in use by another application"
        );
        assert_eq!(field(&line, "reason"), Some(REASON_STREAM_NOT_STARTED));
        assert_eq!(field(&line, "in_process_ms"), Some("88.1"));
        assert_eq!(
            detail(&line),
            Some("the device is already in use by another application")
        );
    }

    /// A prefix that is not a whole field name must not match: `state_params` is a suffix of
    /// `default_state_params`, and a looser matcher would read one as the other.
    #[test]
    fn field_matches_whole_field_names_only() {
        let line = format!("{AUDIBLE_MARKER} default_state_params=27");
        assert_eq!(field(&line, "state_params"), None);
        assert_eq!(field(&line, "default_state_params"), Some("27"));
    }

    /// The one behaviour an ordinary launch depends on: with the variable unset, the probe is off
    /// and hands the config directory decision back to `namir_platform`. (The variable is not set
    /// anywhere in this process, and this test deliberately does not set it — see the first test's
    /// comment.)
    #[test]
    fn the_probe_is_off_when_its_variable_is_unset() {
        assert!(std::env::var_os(PROBE_ENV).is_none());
        assert!(!enabled());
        assert_eq!(config_dir_override(), None);
    }
}
