# 04 — State and preset format (`.namirpreset`)

NFR-DOC-010: "the state and preset format shall be documented to a level that lets a third party
write a compatible reader without reading this project's source." This document is that reference.
Everything here is checked against `crates/namir-state`'s actual implementation as it stands after
M6, not against an aspirational schema — where an earlier design sketch (the M5 plan) differs from
what was actually built, what follows describes the real reader/writer.

See `docs/02-architecture.md` §10/§11 (D-10.4, D-11.1/D-11.2/D-11.3) for the design rationale
behind the choices recorded here as fact.

**M6 (D-10.4) changed this format:** `global.bypass`/`global.output_ceiling_db` moved out of their
own `global` section and into `parameters`, alongside every other parameter. §5 below now
documents that move, including the backward-compatible reading of an older document's `global`
section a writer built before M6 may have produced.

## 1. Scope: presets and plugin state are the same document type

FR-STATE-060 treats "plugin state" (a DAW's own save/restore of an instance) as a superset of "a
preset" (a named, shareable file a user saves/loads deliberately). This format does not distinguish
them: a `.namirpreset` file and a CLAP host's serialised plugin state are the same JSON shape. A
future host-state superset (session-specific fields a preset file would never carry) is expected to
add a new top-level section — `host`, reserved but not yet used by this build — not a second format.

## 2. File extension, encoding, and byte-level guarantees

- Extension: `.namirpreset`.
- Encoding: UTF-8 JSON text, no byte-order mark.
- Line endings: `\n` only. A writer must never emit `\r`.
- Whitespace: pretty-printed, 2-space indentation (`serde_json`'s `to_vec_pretty` convention).
- Object key order: **sorted**, always. This build's JSON library backs every object with a
  `BTreeMap` rather than preserving insertion order, so this is a property of the implementation,
  not a rule a writer has to remember to apply.
- Numbers: written in the shortest decimal form that round-trips exactly back to the same
  floating-point value (the same guarantee `serde_json`'s own float formatter makes). A reader must
  not assume a fixed number of decimal places.
- Maximum document size: 256 MiB (`namir_core::MAX_FILE_BYTES`, NFR-SEC-020's byte ceiling). A
  reader is not required to accept anything larger; this project's own reader rejects it outright,
  before parsing, based on the byte count alone.

A conforming writer must produce output meeting every rule above. A conforming reader must accept
input that satisfies them, and — per §7 below — must not fail merely because the input carries
additional data this document doesn't describe.

## 3. Top-level structure

```json
{
  "format_version": 1,
  "parameters": { "eq.mid_q": 0.7, "global.bypass": 0.0, "global.output_ceiling_db": 0.0, "ir.level_db": -3.0, "...": 0.0 },
  "references": {
    "nam": { "hash": "...", "library_relative": "marshall/plexi.nam", "absolute": "C:\\Users\\erwan\\Models\\plexi.nam", "display_name": "plexi.nam" },
    "ir":  { "hash": "...", "library_relative": "cabs/1960a.wav", "absolute": "/home/erwan/irs/1960a.wav", "display_name": "1960a.wav" }
  }
}
```

Three keys this build writes, and reads as the current shape: `format_version` (§4), `parameters`
(§6, which — since M6's D-10.4 — includes `global.bypass`/`global.output_ceiling_db` alongside
every other parameter; see §5), `references` (§7). `references.nam` and `references.ir` are each
individually optional — a document with no model loaded and no IR loaded omits both, or the whole
`references` object.

A document written by a build **before** M6 also carries a fourth key, `global` — see §5 for that
legacy shape and how a current reader still accepts it.

A document may carry other top-level keys (a `host` block, a `meta` block, anything a future
version or a hand-editor adds). This build preserves them byte-identically across a load-modify-save
cycle without understanding them — see §8.

## 4. `format_version`

An unsigned integer, required.

| Condition | Behaviour |
|---|---|
| Absent, or present but not an integer | **Rejected outright** — this is the one thing this format treats as fatal rather than tolerated, because there is no defensible default for "which schema is this". |
| Equal to this build's version (currently `1`) | Loaded normally. |
| Greater than this build's version | Loaded **tolerantly**, with a warning. A newer-format document is expected to carry fields an older reader doesn't recognise; §8's unknown-field preservation is what makes this safe rather than merely permissive. |
| Less than this build's version | Passed through a migration chain keyed on the version number, then loaded. No migrations exist yet — version `1` is the floor — but the seam exists in the reader for when one is needed. |

## 5. `global.bypass` / `global.output_ceiling_db` (D-10.4)

**Current shape (M6 onward):** these are two ordinary entries under `parameters` (§6) — a
`namir_params::REGISTRY` descriptor exists for each — not a section of their own:

| Key | `parameters` value | Meaning |
|---|---|---|
| `global.bypass` | `0.0` (Off, default) or `1.0` (On) — a `Stepped` parameter's selected index, per §6's own convention | The chain-wide bypass (FR-CHAIN-030). |
| `global.output_ceiling_db` | number, default `0.0` | The output ceiling in dB (FR-CHAIN-090). |

Before D-10.4 (M5), both values had no `ParamDescriptor` and instead lived in a separate top-level
`global` section:

```json
"global": { "bypass": false, "output_ceiling_db": 0.0 }
```

**Backward compatibility.** A reader built at or after M6 still accepts that legacy `global`
section, per D-11.2's tolerant/versioned deserialisation: if `parameters` doesn't itself carry
`global.bypass` and/or `global.output_ceiling_db`, the corresponding value is read from the legacy
section's own `bypass`/`output_ceiling_db` fields instead (each still falling back to its own
default — `false`/`0.0` — if even the legacy section is absent or the field is missing or
wrongly-typed). If a document somehow carries both shapes, the `parameters` entries win. **A
current writer never emits a `global` section** — every save writes `global.bypass`/
`global.output_ceiling_db` into `parameters` only. A legacy `global` section found on a
load-modify-save cycle is preserved byte-for-byte (§8's general unrecognised-section rule) rather
than deleted, but it is inert from that point on: a subsequent load always prefers the
`parameters` entries, which the save just wrote.

A third-party reader that only needs to support documents this build itself produces can ignore
the legacy shape entirely and treat `global.bypass`/`global.output_ceiling_db` as two more
`parameters` keys, exactly as §6 describes every other key. A reader that also needs to open
pre-M6 documents should additionally check for a top-level `global` object and apply its two
fields as a fallback, per the rule above.

## 6. `parameters`

A flat JSON object mapping a **stable string key** (never a numeric id) to a number.

- **Key.** Each key is one of this build's parameter identifiers, e.g. `"eq.mid_q"`,
  `"trim.gain_db"`, `"ir.level_db"`. The complete, authoritative list is this project's checked-in
  `params.lock` manifest — a third-party reader that wants to validate keys against something
  should read that file, not attempt to reconstruct the registry from this document. Once a key
  appears in `params.lock` it is permanent: it is never repurposed for a different control, only
  ever tombstoned if retired.
- **Value, and its unit.** A parameter's value is stored **in its own physical unit** — a gain in
  dB, a frequency in Hz, a ratio as a plain number — **never normalised to 0..1**. A stepped
  parameter (a named choice among a fixed list) stores the **selected index** as a plain integer-
  valued number (e.g. `1.0` for the second option), not its display name and not a normalised
  fraction. Both choices exist for the same reason: FR-STATE-020 requires an old document to keep
  meaning the same thing after this build's ranges or option lists change, and a value expressed
  relative to the *current* range or a cosmetic display name cannot survive that.
- **Absence.** A key this document does not mention takes that parameter's documented default —
  every parameter has one. A conforming reader must apply defaults for every key it doesn't find,
  not merely for the ones it happens to check.
- **Unrecognised keys.** A key this reader does not recognise is preserved (written back verbatim
  on the next save) but **not applied to anything** — see §8. This is what lets an older build open
  a document a newer build saved without losing the newer build's settings on the next save.
- **Out-of-range or malformed values.** A recognised key whose value is a finite number outside
  that parameter's valid range is clamped into range. A recognised key whose value is not a finite
  number at all (wrong JSON type, or literally unrepresentable — JSON itself has no `NaN`/`Infinity`
  token, so this case only arises from a hand-edited or adversarially constructed file) resets that
  one parameter to its default. Neither condition fails the document.

## 7. `references`

An object with up to two keys, `nam` and `ir`, each holding a **file reference** — a record of
identity plus resolution hints for one loaded model or impulse response. Absent means nothing of
that kind is loaded; a stage with no reference here loads empty (FR-STATE-070).

### 7.1 File reference shape

```json
{
  "hash": "3b1f...c41a",
  "library_relative": "marshall/plexi.nam",
  "absolute": "C:\\Users\\erwan\\Models\\plexi.nam",
  "display_name": "plexi.nam",
  "embedded": {
    "encoding": "base64",
    "media_type": "application/vnd.namir.nam+json",
    "data": "eyJmYWtlIjo..."
  }
}
```

| Field | Required | Type | Meaning |
|---|---|---|---|
| `hash` | **yes** | string, 64 lowercase hex characters | The referenced file's BLAKE3 content hash. This is the reference's *identity* (P7: "identity of a model or IR is its content hash, paths are hints") — every other field below is a hint toward *finding* the bytes this hash identifies, and none of them is trusted without a hash match against the bytes actually found. |
| `library_relative` | no | string, `/`-separated | A path relative to *some* configured library root, in the order a resolver's own root list gives it. Never a Windows-separated or absolute string — see §7.3. |
| `absolute` | no | string | The originating platform's absolute path, **verbatim and opaque**. A reader must never parse this structurally (splitting on `/` vs `\`, stripping a drive letter, etc.) — it may be foreign-platform syntax the reading platform cannot interpret at all, and that is an expected, harmless outcome, not an error to work around. |
| `display_name` | no (empty string if absent) | string | The file's display name, for showing "the missing file is called ⟨this⟩" without deriving a name from `absolute`, which would require parsing platform-specific path syntax. |
| `embedded` | no | object | See §7.2. |

If `hash` is missing, or is present but is not a well-formed 64-hex-character string, the whole
reference is malformed: this build's reader treats it as absent (the stage loads empty) with a
warning, rather than failing the whole document over one bad reference.

**Known limitation, worth a third-party reader knowing about explicitly:** unlike the top-level
document (§8), a single file reference object has **no carrier for a field it doesn't recognise**.
A reference written by a future version that adds a sixth field will have that field silently
dropped by this build on a load-modify-save cycle. Accepted for M5 because no such field exists
yet; if this changes, this section will document a carrier the way §8 already does for the document
as a whole.

### 7.2 `embedded` (FR-STATE-080)

An optional inline copy of the referenced resource's own bytes, for the case none of the path-based
candidates below can find it (a preset shared with someone whose library is configured differently,
or no library at all).

| Field | Required | Value |
|---|---|---|
| `encoding` | **yes** | Always `"base64"` — the only encoding this format defines. Any other value is rejected. |
| `media_type` | no | `"application/vnd.namir.nam+json"` for an embedded model, `"audio/wav"` for an embedded IR. Informational only; a reader decides what the bytes are from *which* reference slot (`references.nam` vs `references.ir`) carried them, not by parsing this field. |
| `data` | **yes** | The resource's raw bytes, base64-encoded (standard alphabet, with padding). |

The *encoded* text is subject to the same 256 MiB ceiling as the whole document (§2) — checked
against the encoded string's own length, before any base64 decoding happens, so a maliciously large
`data` string cannot make a reader allocate proportionally to an attacker-chosen size before the
ceiling is enforced.

### 7.3 `library_relative`'s path syntax

Always `/`-separated in the stored document, regardless of which platform wrote it. Rules a value
must satisfy to be well-formed:

- Non-empty.
- Not rooted (no leading `/`) and not drive-prefixed (no `C:`-style prefix) — a library-relative
  path is relative by definition; an absolute one belongs in `absolute` instead.
- No segment is empty, `.`, or `..` — no traversal, no degenerate segments.

A reader on any platform resolves this by joining its own separator convention onto a configured
library root — the stored `/` characters are never platform syntax, so a Windows-authored
`"cabs/1960a.wav"` resolves identically on Linux and vice versa (NFR-PORT-050).

### 7.4 Resolution order (FR-STATE-070)

A reader locating the file a reference names tries, **in this order**, stopping at the first
candidate whose bytes actually hash to `hash`:

1. **`library_relative`**, if present, joined onto each configured library root in that resolver's
   own configured order.
2. **`absolute`**, if present, opened verbatim.
3. **A content-hash search** of the library index (a hash → path lookup built by an independent
   scanning subsystem outside this document's own scope) — always attempted, since `hash` is never
   optional.
4. **`embedded`**, if present, as the final fallback.

**A path or search hit is not accepted on existence alone.** Every candidate's bytes are hashed and
compared against `hash` before being used; a mismatch (the file at that path now holds different
content than when the reference was saved) is treated exactly like that candidate not existing, and
resolution falls through to the next one. This is P7's identity rule made literal: a different amp
loaded silently under an old, stale path is a worse failure than reporting the reference as missing.

If every candidate misses (or mismatches), the reference is unresolved: the stage that would have
loaded it loads empty instead, and a conforming implementation is expected to surface the file's
`display_name` and `hash` to the user with an option to locate it manually.

## 8. Forward- and backward-compatibility (D-11.2)

Two independent guarantees, both load-bearing for FR-STATE-020's "every past version's documents
load" and for the "a project saved by a newer Namir and reopened by an older one does not silently
lose settings" property:

- **Unrecognised top-level sections** (a `host` block a future version defines, or literally
  anything a hand-editor adds) are preserved byte-identically through a load-modify-save cycle,
  even though this build never reads or writes them itself.
- **Unrecognised keys inside a section this build *does* own** (an unrecognised key sitting
  alongside real ones inside `parameters`, most concretely) are *also* preserved through the same
  cycle — not merely at the top level. Saving a document that changes one known parameter must not
  drop an unknown sibling key in the same section.

Both guarantees hold at arbitrary nesting depth within a section this build owns, with the one
stated exception in §7.1 (a single `FileRef` object has no carrier of its own yet).

## 9. Worked example

A complete, minimal document with one model loaded, an embedded copy attached, no IR, and one
parameter changed from its default:

```json
{
  "format_version": 1,
  "parameters": {
    "global.bypass": 0.0,
    "global.output_ceiling_db": 0.0,
    "trim.gain_db": 3.0
  },
  "references": {
    "nam": {
      "display_name": "plexi.nam",
      "embedded": {
        "data": "eyJmYWtlIjoibWluaW1hbCBuYW0tc2hhcGVkIGpzb24gZm9yIGNvcnB1cyBzZWVkaW5nIn0=",
        "encoding": "base64",
        "media_type": "application/vnd.namir.nam+json"
      },
      "hash": "0e2b6f...c41a"
    }
  }
}
```

(This example's `parameters` object omits every other registered key; a real document written by
this build's own writer includes every registered key, each at either its saved or its default
value — an *omitted* key is a reader-side allowance for hand-edited or partial documents, not
something this build's own writer ever produces. See §6's "Absence" row.)

## 10. Writing a minimal compatible reader

For a third party implementing NFR-DOC-010's bar directly — extract every parameter value and both
file references without reading any Rust source:

1. Parse the file as JSON (any conformant parser).
2. Read `format_version` as an integer. Reject if absent or not an integer. If greater than the
   version you support, proceed anyway (§4) rather than rejecting.
3. Iterate `parameters` as a flat string-to-number map, which includes `global.bypass` (`0.0`/
   `1.0`) and `global.output_ceiling_db` (number) alongside every other parameter (§5). Every key
   not in your own list of known parameters is data you should still retain (for a round-trip
   writer) but not act on.
4. If you also need to open documents written before M6 (D-10.4): check for a top-level `global`
   object and, for whichever of `global.bypass`/`global.output_ceiling_db` step 3 didn't find in
   `parameters`, read it from there instead — `bypass` (boolean, default `false`) and
   `output_ceiling_db` (number, default `0.0`), tolerating a missing or wrongly-typed `global`
   object entirely (treat as both defaults). A reader that only needs to open documents this
   build's own current writer produces can skip this step.
5. For each of `references.nam` and `references.ir`, if present: read `hash` (required — treat the
   whole reference as absent if missing or malformed), and optionally `library_relative`,
   `absolute`, `display_name`, `embedded.data` (base64-decode it if you need the bytes directly).
   Apply §7.4's resolution order and its hash-verification rule if you intend to actually locate
   and open the referenced file rather than merely read the reference's own fields.

This document's own §7.4 and §8 are the two sections most likely to be under-implemented by a
reader built from intuition alone — the hash-verification-before-trusting-a-path-hit rule and the
unknown-field preservation rule are both easy to miss and both load-bearing for this format's
stated guarantees.
