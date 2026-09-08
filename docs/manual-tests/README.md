# Manual-test documents — the verdict convention

Every file in this directory is one requirement's manual test: the script a human runs, and the
record of what happened when someone ran it. `xtask traceability` reads these files, so the record
has to be machine-readable as well as readable.

This file is the convention. It is the one file here that is *not* a manual test, and the only name
`xtask` exempts from the rule below (`VERDICT_EXEMPT_FILES`, `xtask/src/traceability.rs`).

## The rule

**Every document in this directory must carry at least one verdict line.** A verdict line is a line
whose text *begins* `**Result:` and whose first words after that marker are one of exactly four
tokens, in upper case:

| Token | Means |
|---|---|
| `PASS` | The whole script was run, and every step passed. |
| `FAIL` | The script was run and something it asserts does not hold. |
| `PARTIAL` | Some of the script was run; some of it was not, or some of it failed. |
| `NOT EXECUTED` | None of the script has been run. |

Everything after the token is ordinary prose — say what was run, on what machine, by whom, and what
was not. The token is what the tool reads; the prose is what the next person reads.

```markdown
**Result: PASS.** All six steps executed on the §2 reference machine, 2026-08-27.

**Result: PARTIAL.** Steps 1-2 executed; step 3 needs a display and was not run.

**Result: NOT EXECUTED this session (no Linux/macOS hardware available).**
```

Four consequences worth knowing before you write one:

- **Only `PASS` credits its requirement.** For a `Verify: M` Must, this document *is* the traced
  artifact (D-18.6), so anything else leaves that requirement uncovered in `docs/03-test-plan.md`
  and in the gate's own uncovered list. That is the point: before M15 the gate matched a filename
  and printed `clean -- all 130 Must requirements are covered` while six of those Musts' scripts
  recorded `NOT EXECUTED`, `PARTIAL` or `FAIL` (issue #34).
- **A missing or malformed verdict is a hard error** that aborts the whole `xtask traceability`
  run, in the same way and for the same reason a malformed `// trace:` annotation does: it is a bad
  input, not a coverage gap, and no flag — `--write` and `--allow-uncovered` included — reaches past
  it.
- **The worst verdict in a document wins**, not the first. A document may carry more than one
  verdict line (a later run recording one step of a script the rest of which is still unexecuted);
  the gate takes the least favourable.
- **The worst verdict across a requirement's documents wins too** (added 2026-09-08). A requirement
  can be matched by more than one document — by filename prefix, or by a document's
  `**Requirement (literal…):**` block declaring its id — and every match is read, so one script's
  `PASS` never speaks for a sibling script's unexecuted steps. Until this pass the lookup stopped
  at the first match, and PR #159's two new scripts, `fr-io-010-device-selection.md` and
  `fr-io-040-sample-rate-and-buffer-size.md`, were invisible: `fr-io-010-device-enumeration.md`
  sorts first, declares both ids and records a `PASS` earned against the pre-panel surface, so both
  Musts read plainly covered while the scripts written for the surface that had just shipped had
  never been run. **Practical consequence when you add a document:** a new script for an existing
  requirement takes that requirement uncovered until it is executed. That is the intended
  behaviour, not a regression — write the script anyway.
- **`PASS` may not contradict itself.** A `PASS` line that goes on to say some part was `NOT
  EXECUTED` / `NOT RUN` is refused, not quietly downgraded. Write `PARTIAL` and keep the sentence.

## What does not change

Recording a `FAIL`, a `PARTIAL` or a `NOT EXECUTED` honestly is the normal, expected state of a
document here, and has been for milestones at a time — see this directory's own history. Never
promote a verdict to make a gate green. Run the script, or leave the record as it is and let the
requirement read uncovered.
