## Summary

<!-- What changed and why. One paragraph. The diff shows how. -->

## Verification

<!-- Which gates were actually run, and on what. Delete the lines you did not run;
     do not tick a box for a command you did not type. A number measured off
     docs/02-architecture.md §2's reference machine is informational, never certified. -->

- [ ] The gate block in `README.md` (all `xtask` subcommands + `cargo deny check`), which
      `xtask ci-commands` keeps in step with `.github/workflows/ci.yml` in both directions
- [ ] `cargo run -p xtask -- traceability` (with `--allow-uncovered` until M14's close-out, when
      D-18.5 makes the zero-uncovered half required and the flag is deleted)
- [ ] Manual test executed (name the file under `docs/manual-tests/`)

## Requirements and documents

<!-- FR-*/NFR-* touched; any `trace:` / `trace-partial:` tag added, moved or demoted;
     any appended *Consequence* / status / close-out subsection.
     "None" is a valid and common answer. -->
