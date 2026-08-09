//! FR-PARAM-020 / D-10.1: "a checked-in parameter manifest is diffed in CI; a changed or reused
//! identifier fails the build." `namir_params::render_manifest(namir_params::REGISTRY)` is the
//! source of truth; this module compares it against the checked-in root `params.lock` and, with
//! `--write`, regenerates the file in place (the same operation
//! `namir-params`'s own `#[ignore] generate_params_lock` test performs, exposed here as a
//! developer-facing command instead of a one-shot test).

// trace-partial: FR-PARAM-020
// uncovered: FR-PARAM-020 — the reused-identifier half of the method has no artifact in the gate:
// uncovered: check_manifest, the function that detects TOMBSTONE_REUSED and ID_CHANGED, has no
// uncovered: caller outside its own test module, and the byte-equality check CI runs against
// uncovered: render_manifest's live-only output makes a tombstoned line in params.lock fail the
// uncovered: gate permanently and be deleted by --write; closes M9b

use std::path::Path;

/// Compares `params.lock` under `repo_root` against `render_manifest(REGISTRY)`. `Ok(true)`
/// means the file was up to date (or, with `write = true`, has just been made so); `Ok(false)`
/// means it was stale and `write` was `false`, so the file was left untouched. The returned
/// `String` is a human-readable status/diff message for CI logs either way.
pub fn check_or_write(repo_root: &Path, write: bool) -> Result<(bool, String), String> {
    let expected = namir_params::render_manifest(namir_params::REGISTRY);
    let lock_path = repo_root.join("params.lock");

    if write {
        std::fs::write(&lock_path, &expected)
            .map_err(|e| format!("failed to write {}: {e}", lock_path.display()))?;
        return Ok((true, format!("wrote {}", lock_path.display())));
    }

    let actual = std::fs::read_to_string(&lock_path)
        .map_err(|e| format!("failed to read {}: {e}", lock_path.display()))?;

    if actual == expected {
        return Ok((true, format!("{} is up to date", lock_path.display())));
    }

    Ok((false, diff_message(&lock_path, &actual, &expected)))
}

fn diff_message(lock_path: &Path, actual: &str, expected: &str) -> String {
    let actual_lines: Vec<&str> = actual.lines().collect();
    let expected_lines: Vec<&str> = expected.lines().collect();

    let mut message = format!(
        "{} is stale -- does not match render_manifest(REGISTRY). Run \
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

    #[test]
    fn stale_file_is_reported_without_write() {
        let dir = std::env::temp_dir().join(format!(
            "xtask-params-lock-stale-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("params.lock"), "not the real manifest\n").unwrap();

        let (up_to_date, message) = check_or_write(&dir, false).unwrap();
        assert!(!up_to_date);
        assert!(message.contains("stale"));

        fs::remove_dir_all(&dir).ok();
    }
}
