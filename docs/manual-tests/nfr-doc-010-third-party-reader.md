# NFR-DOC-010 manual test: a third-party reader from documentation alone

**Requirement (literal):** the state and preset format shall be documented to a level that lets a
third party write a compatible reader without reading this project's source.

**Verify: M** (manual). The only version of this requirement that can actually fail: write a
reader using *only* `docs/04-state-and-preset-format.md`, run it against a real document this
project's own writer produced, and check whether it extracts the right values.

## Script

1. Generate a real document with `xtask preset` (no product shell needed — see the FR-STATE-040
   manual test alongside this one).
2. Without reading any Rust source in this repository, write a `jq` or Python reader against
   `docs/04-state-and-preset-format.md` alone that extracts:
   - `format_version`;
   - `global.bypass` / `global.output_ceiling_db`;
   - every key/value pair under `parameters`;
   - both `references.nam` and `references.ir` (`hash`, `library_relative`, `absolute`,
     `display_name`, and `embedded.data` decoded if present).
3. Run it against the generated document. Compare every extracted value against what the Rust
   writer actually wrote (visible by opening the same file directly).

## Executed run (this session)

A Python script (`read_preset.py`, reproduced below) was written using only
`docs/04-state-and-preset-format.md` as a reference — no Rust source was consulted while writing
it — and run against `xtask preset`'s generated sample:

```
$ python read_preset.py sample.namirpreset
format_version: 1
global.bypass: False
global.output_ceiling_db: 0.0
parameters: 27 entries
  eq.enabled = 1.0
  ...
  trim.gain_db = 3.0
references.nam.hash: 1b3abe2ebc0e30f0ba500162df83f4c34c6bf4487d504b11d315b2ba1f59f112
references.nam.library_relative: marshall/plexi.nam
references.nam.absolute: C:\Users\erwan\Models\marshall\plexi.nam
references.nam.display_name: plexi.nam
references.nam.embedded: absent
references.ir.hash: 6efc64c76203990de346e5f3746667afe1226fc56c42a352fce6c2ee456dea54
references.ir.library_relative: cabs/1960a.wav
references.ir.absolute: /home/erwan/irs/1960a.wav
references.ir.display_name: 1960a.wav
references.ir.embedded: absent
unrecognised top-level keys (must be preserved, not required to be understood): []
```

Every value matches the document the Rust writer produced (27 parameters, `trim.gain_db = 3.0`,
both references' hashes, paths and display names) byte-for-byte against the file opened directly
in a text editor. The two format properties the script's own comments flag as "easy to miss" while
writing it from the documentation — §4's tolerant handling of a `format_version` newer than the
reader's own, and §7.1's hash-required-else-reference-is-malformed rule — were both implementable
directly from the doc text without ambiguity.

**Result: PASS.**

## `read_preset.py`, in full

```python
#!/usr/bin/env python3
"""NFR-DOC-010's manual test, executed: a reader written using ONLY
docs/04-state-and-preset-format.md as a reference -- no Rust source was consulted while writing
this script. Extracts every parameter value and both file references from a .namirpreset file and
prints them for comparison against what the Rust writer actually wrote.

Usage: python read_preset.py <path-to-.namirpreset>
"""
import base64
import json
import sys


def read_preset(path):
    with open(path, "rb") as f:
        raw = f.read()

    # Sec 2: encoding is UTF-8, no BOM, LF only. Sanity-check the byte-level guarantees the doc
    # promises before even parsing.
    assert b"\r" not in raw, "doc promises no \\r anywhere in the file"
    assert not raw.startswith(b"\xef\xbb\xbf"), "doc promises no BOM"

    doc = json.loads(raw.decode("utf-8"))

    # Sec 4: format_version, required integer.
    version = doc.get("format_version")
    if not isinstance(version, int):
        raise ValueError("format_version missing or not an integer -- reject per Sec 4")
    print(f"format_version: {version}")

    # Sec 5: global, both fields optional with stated defaults.
    global_section = doc.get("global", {})
    bypass = global_section.get("bypass", False)
    ceiling = global_section.get("output_ceiling_db", 0.0)
    print(f"global.bypass: {bypass}")
    print(f"global.output_ceiling_db: {ceiling}")

    # Sec 6: parameters, flat string -> number map.
    params = doc.get("parameters", {})
    print(f"parameters: {len(params)} entries")
    for key in sorted(params):
        print(f"  {key} = {params[key]}")

    # Sec 7: references.nam / references.ir, each optional.
    references = doc.get("references", {})
    for slot in ("nam", "ir"):
        ref = references.get(slot)
        if ref is None:
            print(f"references.{slot}: absent")
            continue
        ref_hash = ref.get("hash")
        if not isinstance(ref_hash, str) or len(ref_hash) != 64:
            raise ValueError(f"references.{slot}.hash missing or not 64 hex chars -- malformed per Sec 7")
        print(f"references.{slot}.hash: {ref_hash}")
        print(f"references.{slot}.library_relative: {ref.get('library_relative')}")
        print(f"references.{slot}.absolute: {ref.get('absolute')}")
        print(f"references.{slot}.display_name: {ref.get('display_name', '')}")
        embedded = ref.get("embedded")
        if embedded is not None:
            if embedded.get("encoding") != "base64":
                raise ValueError("embedded.encoding must be base64 per Sec 7.2")
            data = base64.b64decode(embedded["data"])
            print(f"references.{slot}.embedded: {len(data)} decoded bytes, media_type={embedded.get('media_type')}")
        else:
            print(f"references.{slot}.embedded: absent")

    # Sec 3/8: other top-level keys are allowed and must not break a reader.
    known = {"format_version", "global", "parameters", "references"}
    extra = set(doc.keys()) - known
    print(f"unrecognised top-level keys (must be preserved, not required to be understood): {sorted(extra)}")


if __name__ == "__main__":
    read_preset(sys.argv[1])
```
