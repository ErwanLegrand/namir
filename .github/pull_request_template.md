## Summary

<!-- What changed and why. One paragraph. The diff shows how. -->

## Verification

<!-- Which gates were actually run, and on what. Delete the lines you did not run;
     do not tick a box for a command you did not type. A number measured off
     docs/02-architecture.md §2's reference machine is informational, never certified. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace --no-fail-fast`
- [ ] `cargo run -p xtask -- layering` / `rt-logging` / `params-lock` / `attribution` / `identity`
- [ ] `cargo run -p xtask -- traceability --allow-uncovered`
- [ ] `cargo deny check`
- [ ] Manual test executed (name the file under `docs/manual-tests/`)

## Requirements and documents

<!-- FR-*/NFR-* touched; any `trace:` / `trace-partial:` tag added, moved or demoted;
     any appended *Consequence* / status / close-out subsection.
     "None" is a valid and common answer. -->
