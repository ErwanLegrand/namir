//! FR-PARAM-020 / D-10.1: "a checked-in parameter manifest is diffed in CI; a changed or reused
//! identifier fails the build." This module is that gate: it reconciles the checked-in root
//! `params.lock` against `namir_params::REGISTRY` and, with `--write`, regenerates the file in
//! place.
//!
//! # Both halves of the method, and why the second was missing until M14
//!
//! D-10.1 states two obligations, not one: the manifest is *diffed*, **and** "a changed or reused
//! identifier fails the build". Until M14 this module implemented only the first, and implemented
//! it against `render_manifest(REGISTRY)` — the **live-only** render. Two consequences followed,
//! and together they made D-10.1's retirement mechanism inoperable (issue #31):
//!
//! - [`namir_params::check_manifest`], the function that detects `TOMBSTONE_REUSED`, `ID_CHANGED`,
//!   `KIND_CHANGED` and `DROPPED`, had **no caller outside its own test module**. The half of
//!   FR-PARAM-020 that actually protects a parameter's identity ran nowhere.
//! - A hand-flipped `tombstoned` line — the one edit D-10.1 permits to `params.lock`, and the whole
//!   of how a parameter is retired — is by construction absent from a live-only render, so it made
//!   the byte-equality check fail permanently, and `--write` deleted it.
//!
//! Both are closed here. The comparison target is now
//! [`namir_params::merge_manifest`]`(the checked-in file, REGISTRY)` — the live render *plus* the
//! tombstones already in the file — and `check_manifest` runs before either branch, in **both**
//! modes. Running it under `--write` is the load-bearing half of that: `--write` must never be able
//! to regenerate its way past a reused identifier, which is precisely the failure a tombstone
//! exists to make impossible.
//!
//! With no tombstone in the file, `merge_manifest` is byte-identical to `render_manifest`, so this
//! change moves nothing in today's `params.lock` beyond its header text.

// The gate now executes both of the method's conjuncts against the real REGISTRY and the real
// checked-in file: `params_lock_gate_*` below drive the diff, the tombstone round-trip and each
// violation class the checker can raise.
// trace: FR-PARAM-020

use std::path::Path;

/// Compares `params.lock` under `repo_root` against
/// [`namir_params::merge_manifest`]`(that file, REGISTRY)`, having first required the file to
/// satisfy [`namir_params::check_manifest`]. `Ok(true)` means the file was up to date (or, with
/// `write = true`, has just been made so); `Ok(false)` means it was stale and `write` was `false`,
/// so the file was left untouched. `Err` is a check the gate could not run *or* a manifest-rule
/// violation — the latter is an `Err` rather than an `Ok(false)` because there is no regeneration
/// that fixes it: a reused id is a source change to reverse, not a stale file to rewrite.
///
/// The returned `String` is a human-readable status/diff message for CI logs either way.
pub fn check_or_write(repo_root: &Path, write: bool) -> Result<(bool, String), String> {
    let lock_path = repo_root.join("params.lock");

    // Absent is legitimate only under `--write` (first generation, and the shape this module's own
    // round-trip test exercises); a check against a file that is not there is a failure, as before.
    let actual = match std::fs::read_to_string(&lock_path) {
        Ok(text) => text,
        Err(_) if write => String::new(),
        Err(e) => return Err(format!("failed to read {}: {e}", lock_path.display())),
    };

    // Before both branches, deliberately. See the module doc: `--write` regenerating past a reused
    // identifier would defeat the mechanism this check exists to defend.
    if let Err(violations) = namir_params::check_manifest(&actual, namir_params::REGISTRY) {
        let mut message = format!(
            "{} violates D-10.1's identifier rules. This is not a staleness a regeneration \
             fixes -- revert the source change, or retire the parameter by flipping its existing \
             line to `tombstoned` (never by deleting it, and never by reusing its id):\n",
            lock_path.display()
        );
        for violation in &violations {
            message.push_str(&format!("  - {violation}\n"));
        }
        return Err(message);
    }

    let expected = namir_params::merge_manifest(&actual, namir_params::REGISTRY);

    if write {
        std::fs::write(&lock_path, &expected)
            .map_err(|e| format!("failed to write {}: {e}", lock_path.display()))?;
        return Ok((true, format!("wrote {}", lock_path.display())));
    }

    if actual == expected {
        return Ok((true, format!("{} is up to date", lock_path.display())));
    }

    Ok((false, diff_message(&lock_path, &actual, &expected)))
}

fn diff_message(lock_path: &Path, actual: &str, expected: &str) -> String {
    let actual_lines: Vec<&str> = actual.lines().collect();
    let expected_lines: Vec<&str> = expected.lines().collect();

    let mut message = format!(
        "{} is stale -- does not match merge_manifest(this file, REGISTRY). Run \
         `cargo run -p xtask -- params-lock --write` to regenerate it.\n",
        lock_path.display()
    );

    for line in &actual_lines {
        if !expected_lines.contains(line) {
            message.push_str(&format!("- {line}\n"));
        }
    }
    for line in &expected_lines {
        if !actual_lines.contains(line) {
            message.push_str(&format!("+ {line}\n"));
        }
    }

    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn diff_message_reports_lines_present_only_on_one_side() {
        let actual = "a\nb\nc\n";
        let expected = "a\nb\nd\n";
        let message = diff_message(Path::new("params.lock"), actual, expected);
        assert!(message.contains("- c"));
        assert!(message.contains("+ d"));
        assert!(!message.contains("- a"));
        assert!(!message.contains("+ a"));
    }

    #[test]
    fn write_then_check_round_trips() {
        let dir =
            std::env::temp_dir().join(format!("xtask-params-lock-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let (wrote_ok, _) = check_or_write(&dir, true).unwrap();
        assert!(wrote_ok);

        let (up_to_date, message) = check_or_write(&dir, false).unwrap();
        assert!(up_to_date, "expected up to date, got: {message}");

        fs::remove_dir_all(&dir).ok();
    }

    /// A scratch repo root with `params.lock` written from `text`.
    fn scratch(name: &str, text: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("xtask-params-lock-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        if !text.is_empty() {
            fs::write(dir.join("params.lock"), text).unwrap();
        }
        dir
    }

    fn live_manifest() -> String {
        namir_params::merge_manifest("", namir_params::REGISTRY)
    }

    #[test]
    fn stale_file_is_reported_without_write() {
        // Stale in the one way that is *not* also a D-10.1 violation: a parameter present in
        // REGISTRY but not yet in the file, which is exactly what adding a parameter looks like
        // before regeneration. A malformed or truncated file is now an `Err` instead (below), so
        // this test can no longer use one.
        let text = live_manifest();
        let without_first_param: String = text
            .lines()
            .filter(|line| !line.starts_with("eq.enabled "))
            .map(|line| format!("{line}\n"))
            .collect();
        let dir = scratch("stale", &without_first_param);

        let (up_to_date, message) = check_or_write(&dir, false).unwrap();
        assert!(!up_to_date);
        assert!(message.contains("stale"), "{message}");
        assert!(message.contains("+ eq.enabled"), "{message}");

        fs::remove_dir_all(&dir).ok();
    }

    // --- FR-PARAM-020's second conjunct, end to end (M14, issue #31) ---------------------------
    //
    // "A changed or reused identifier fails the build", and its corollary that a retired one
    // survives. Every test below drives the real gate entry point against a real `params.lock`
    // built from the real `REGISTRY`; none of them stubs `check_manifest`.

    /// `manifest` with the parameter named by `key` retired: its line hand-flipped from `live` to
    /// `tombstoned`, and — since retiring it means deleting its descriptor — REGISTRY no longer
    /// containing it. REGISTRY is a `const` and cannot be edited here, so `retire` fakes the second
    /// half by tombstoning a key REGISTRY *does* carry: that state is `TOMBSTONE_REUSED`, which is
    /// the negative control. For the positive case a key REGISTRY has never carried is used.
    fn with_line(manifest: &str, line: &str) -> String {
        // Appended rather than inserted in key order: where the line sits is irrelevant to the
        // parser, and the merge re-sorting it into place is itself part of what is being checked.
        format!("{manifest}{line}\n")
    }

    #[test]
    fn params_lock_gate_keeps_a_committed_tombstone_green_and_write_no_longer_deletes_it() {
        // The end-to-end verification issue #31 asks for. `zz.retired_example` is a key REGISTRY
        // has never carried, standing in for a parameter that once existed and has been retired:
        // its line is in the file, flipped to `tombstoned`, and its descriptor is gone from the
        // live set.
        let tombstone = "zz.retired_example 4242424242 continuous tombstoned";
        let dir = scratch("tombstone", &with_line(&live_manifest(), tombstone));

        // 1. The gate is green with the tombstone committed. (Before M14 this failed permanently:
        //    the comparison target was the live-only render, which by construction has no
        //    tombstone line.)
        let (up_to_date, message) = check_or_write(&dir, false).unwrap();
        assert!(up_to_date, "{message}");

        // 2. `--write` does not delete it.
        assert!(check_or_write(&dir, true).unwrap().0);
        let after = fs::read_to_string(dir.join("params.lock")).unwrap();
        assert!(after.contains(tombstone), "{after}");

        // 3. And the file is still green afterwards, so the two commands agree.
        assert!(check_or_write(&dir, false).unwrap().0);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn params_lock_gate_rejects_a_reused_tombstoned_identifier_in_both_modes() {
        // The half of FR-PARAM-020 that protects a saved project: a key the manifest already
        // retired coming back live. `--write` is checked too -- a regeneration flag that could
        // rewrite its way past this would make the tombstone decorative.
        let mut text = live_manifest();
        text = text.replace("trim.gain_db 1371108501 continuous live", "");
        let dir = scratch(
            "reuse",
            &with_line(&text, "trim.gain_db 1371108501 continuous tombstoned"),
        );

        for write in [false, true] {
            let err = check_or_write(&dir, write).expect_err("a reused tombstone must fail");
            assert!(
                err.contains("params.manifest.tombstone_reused"),
                "write={write}: {err}"
            );
        }
        // Nothing was written: the file still carries the tombstone it started with.
        let after = fs::read_to_string(dir.join("params.lock")).unwrap();
        assert!(
            after.contains("trim.gain_db 1371108501 continuous tombstoned"),
            "{after}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn params_lock_gate_rejects_a_changed_identifier() {
        // The other clause of the same sentence. A key whose recorded id no longer matches what
        // its key derives is `ID_CHANGED` -- the failure that silently corrupts every saved
        // project that used it.
        let text = live_manifest().replace(
            "trim.gain_db 1371108501 continuous live",
            "trim.gain_db 999999999 continuous live",
        );
        let dir = scratch("id-changed", &text);
        let err = check_or_write(&dir, false).expect_err("a changed id must fail");
        assert!(err.contains("params.manifest.id_changed"), "{err}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn params_lock_gate_rejects_a_silently_dropped_parameter() {
        // Retiring by deletion rather than by tombstone: `DROPPED`. The manifest records a key as
        // `live` that REGISTRY no longer declares, which is what deleting a descriptor without
        // flipping its line looks like. This is the edit the gate has to refuse for the tombstone
        // to be the only route out.
        let dir = scratch(
            "dropped",
            &with_line(
                &live_manifest(),
                "zz.retired_example 4242424242 continuous live",
            ),
        );
        let err = check_or_write(&dir, false).expect_err("a silent drop must fail");
        assert!(err.contains("params.manifest.dropped"), "{err}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn params_lock_gate_rejects_a_malformed_line_rather_than_regenerating_over_it() {
        // `params.lock` is hand-edited to retire a parameter, so a typo in that edit is the
        // expected mistake. It must be reported against its own text, not quietly dropped by a
        // `--write` that then reports success.
        let dir = scratch(
            "malformed",
            &with_line(&live_manifest(), "not a manifest line"),
        );
        for write in [false, true] {
            let err = check_or_write(&dir, write).expect_err("a malformed line must fail");
            assert!(
                err.contains("params.manifest.malformed_line"),
                "write={write}: {err}"
            );
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_real_checked_in_params_lock_passes_the_gate() {
        // The gate as CI runs it, against the real repository root -- the positive control every
        // negative case above needs.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let (ok, message) = check_or_write(&root, false).unwrap();
        assert!(ok, "{message}");
    }
}
