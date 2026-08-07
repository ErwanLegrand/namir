# FR-STATE-040 manual test: diffability and hand-editability

**Requirement (literal):** the state/preset format shall be a text format a user can read,
hand-edit, and diff meaningfully — not merely "happens to be JSON".

**Verify: M** (manual). No product shell exists yet (M6+), so this is executed against
`xtask preset`, the non-UI path M5 built specifically so this script would be runnable — see
`docs/04-state-and-preset-format.md` and that subcommand's own doc comment.

## Script

1. Generate a sample document with no product shell running:
   ```
   cargo run -p xtask -- preset sample.namirpreset
   ```
2. Open `sample.namirpreset` in a plain text editor. Confirm:
   - it is legible, indented JSON, not a single unbroken line or a binary blob;
   - object keys are in a stable, readable (sorted) order, not scrambled between opens;
   - every parameter key is a recognisable name (`"trim.gain_db"`, `"eq.mid_q"`), not an opaque
     numeric id.
3. Copy the file, hand-edit exactly one value in the copy (e.g. change `"trim.gain_db": 3.0` to
   `"trim.gain_db": 6.0`) using the same text editor, save.
4. Diff the original against the hand-edited copy. Confirm the diff touches **only** the one line
   changed — no reordering, no reformatting, no unrelated churn.
5. Load the hand-edited copy back through this project's own reader and confirm the edited value
   took effect, with no warnings:
   ```
   cargo run -p xtask -- preset --verify sample.namirpreset
   ```

## Executed run (this session)

Step 1 — generated (1432 bytes):

```
$ cargo run -p xtask -- preset /tmp/sample.namirpreset
preset: wrote C:/Users/micro/AppData/Local/Temp/sample.namirpreset (1432 bytes)
```

Step 2 — inspected directly (excerpt); pretty-printed, sorted keys, named parameters, exactly as
`docs/04-state-and-preset-format.md` §2/§6 describe:

```json
{
  "format_version": 1,
  "global": { "bypass": false, "output_ceiling_db": 0.0 },
  "parameters": {
    "eq.enabled": 1.0,
    ...
    "trim.gain_db": 3.0
  },
  "references": { "ir": { ... }, "nam": { ... } }
}
```

Steps 3–4 — hand-edited `trim.gain_db` from `3.0` to `6.0` via `sed` (standing in for a text
editor's own save), then diffed:

```diff
--- sample_before.namirpreset
+++ sample.namirpreset
@@ -31,7 +31,7 @@
     "nam.enabled": 1.0,
     "out.gain_db": 0.0,
     "trim.dc_blocker_enabled": 1.0,
-    "trim.gain_db": 3.0
+    "trim.gain_db": 6.0
   },
   "references": {
     "ir": {
```

**One line changed.** Nothing else moved.

Step 5 — loaded the hand-edited file back through the real reader:

```
$ cargo run -p xtask -- preset --verify /tmp/sample.namirpreset
preset --verify: C:/Users/micro/AppData/Local/Temp/sample.namirpreset loaded successfully
  global.bypass = false
  global.output_ceiling_db = 0
  trim.gain_db = Some(6.0)
  eq.mid_q = Some(0.7)
  ir.level_db = Some(-3.0)
  nam reference = Some("plexi.nam")
  ir reference = Some("1960a.wav")
  warnings: none
```

`trim.gain_db` reads back as `6.0` — the edit took effect — with zero warnings and every other
value untouched.

**Result: PASS.**
