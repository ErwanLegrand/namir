# NFR-QUAL-050 manual check: does CI actually gate merges to `trunk`?

**Requirement (literal):** CI shall run the full test suite on every tier-1 and tier-2 platform for
every change, **and shall gate merges**. *Verify:* S.

**This document is supplementary evidence, not the traced artifact** (D-18.6). NFR-QUAL-050 is a
`Verify: S` Must, and `xtask traceability` resolves every code other than `M` against source and
configuration only — a manual document is structurally invisible to it for this requirement, however
much executed evidence it records. The traced artifact stays the `# trace-partial:` at
`.github/workflows/ci.yml`'s `build-test` job, which asserts the platform-matrix half and declares
the merge-gating half unspanned. This file is the record of the half no annotation can reach.

## Why this needs a human at all

Branch protection is a repository *setting*, held outside the repository. No `xtask` subcommand and
no workflow can observe it: the traceability scanner reads checked-in files (`crates/**`, `xtask/**`,
two workflow files, `Cargo.toml`, `deny.toml`), and a ruleset is none of those. A workflow could in
principle query the API for its own repository, but it would be asserting the configuration that
decides whether its own failure blocks anything — a check that cannot fail closed. So the only
verification available is a person reading the setting and writing down what they saw.

## Script

1. Open Settings → Rules → Rulesets for `ErwanLegrand/namir` (and Settings → Branches, since a
   classic branch-protection rule and a ruleset are separate mechanisms and either can carry the
   requirement).
2. Record every rule targeting `trunk`.
3. Assert a required-status-checks rule exists, and that its check list names every **blocking** job
   of `.github/workflows/ci.yml` and `.github/workflows/fuzz.yml`. Matrix legs report as separate
   check names and must each be listed:

   - `build + test (windows-latest)`, `build + test (ubuntu-latest)`, `build + test (macos-latest)`
   - `layering + rt-logging + params.lock + attribution + identity` — the job carrying `layering`,
     `rt-logging`, `network-free`, `error-catalogue`, `feature-guard`, `params-lock`, `assets`,
     `attribution`, `identity`, `schema`, `ci-commands`, and the **required** traceability step
   - `headless window smoke (FR-UI-010, issue #143)`
   - `cargo-deny license audit`
   - `cargo-deny network-free build (D-18.2)`
   - `clap-validator (FR-CLAP-020)`
   - `bundle + inspect the produced distribution (windows-latest)`, `(ubuntu-latest)`,
     `(macos-latest)`
   - `MSRV check`
   - `build with no C++ compiler (NFR-PORT-040)`
   - `mobile cross-build (aarch64-linux-android)`, `mobile cross-build (aarch64-apple-ios)`
   - `fuzz smoke (load_nam, 60s)`, `fuzz smoke (read_state, 60s)`, `fuzz smoke (probe_wav, 60s)`

4. Assert the **informational** checks are *not* required, deliberately: `coverage (informational)`,
   `NFR-PERF-010 chain benchmark (informational)`, `NFR-PERF-030 start-up benchmark (informational,
   cannot certify)`. The second traceability step is `continue-on-error` inside an already-required
   job and so cannot be required separately in any case (D-18.5); requiring it is what M13/M14's
   close-out flip replaces, and that flip is a two-line code change, not a settings change.
5. Assert a pull-request requirement exists, so the required checks are actually interposed rather
   than bypassable by a direct push.
6. Re-run this script whenever a **blocking** job is added to, removed from or renamed in either
   workflow — a required check name that never reports blocks every merge, and a new blocking job
   that nobody adds to the list is advisory in exactly the way this requirement forbids.

## Executed run — 2026-09-07, issue #29

**Result: PARTIAL.** Steps 1-5 were executed by the maintainer, who reports having updated `trunk`'s
ruleset to require the full list above, the three `fuzz.yml` jobs included. Recorded here as
reported: the author of this document has no API access to the rulesets endpoint from the session
that wrote it (`GET /repos/ErwanLegrand/namir/rules/branches/trunk` returns 403 for its token), so
the setting is attested rather than independently re-read, and no screenshot or ruleset JSON is
pasted here. Step 6 is by construction never "executed" — it is the standing obligation this
document exists to carry.

The finding that opened issue #29 was, on the maintainer's reading, correct: before this change
`trunk`'s ruleset carried `deletion` and `non_fast_forward` only, with no required status checks and
no pull-request rule, so a change could land on `trunk` with CI red or unrun. Every gate this project
owns — the traceability ratchet, the layering lint, `params.lock`, attribution, identity,
`cargo-deny`, `clap-validator` — was advisory for that entire period. Nothing is known to have
landed red; that is a statement about what happened to be true, not about what was enforced.

## What is still not closed, and why the tag stays `trace-partial`

Turning the setting on satisfies the requirement in fact. It does not make the requirement
*verifiable* from inside the repository, and D-23.1 asks the annotation to claim only what its
artifact executes:

- nothing re-reads the ruleset, so it can be narrowed or removed silently, and this document would
  still read `PARTIAL` from 2026-09-07 forever;
- step 6's obligation is carried by review alone, the same weakness NFR-QUAL-020 is Partial for;
- the honest closing milestone is **M8**, whose exit checklist is where a human adjudicates evidence
  of this shape. NFR-QUAL-050 cannot be promoted to a plain `# trace:` by any work inside this
  repository — only by amending the requirement's own text or its `Verify:` method.
