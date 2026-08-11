//! M12's product-identity gate: one subcommand covering the three artifacts that carry Namir's
//! identity in the repository — the brand mark `namir-ui` renders, the `README.md` a reader of
//! this repository lands on, and the `TRADEMARK.md` that separates the brand from the code
//! licence.
//!
//! Same generate-and-diff shape as `params_lock.rs` and `attribution.rs`, with one difference
//! worth stating: the generated artifact is **binary**, not text. `images/namir.png` is the source
//! of truth; [`render_blob`] reduces it to an 8-bit alpha mask; [`check`] byte-compares that
//! against the checked-in `crates/namir-ui/src/brand/namir_mark.alpha`, or [`write_blob`]
//! regenerates it with `--write`. The other two checks are pure substring assertions over the two
//! Markdown documents, which is all a `Verify: S` static check can be for a prose artifact.
//!
//! # Why an alpha mask rather than the PNG itself
//!
//! The artwork is a single fill (`#ff6600`) on a transparent background, so every bit of shape and
//! anti-aliasing information in it lives in the alpha channel: an 8-bit alpha mask plus one
//! compile-time colour constant is a *colour-lossless* re-encoding of it. The three channels that
//! are dropped carry nothing [`MARK_FILL`] does not already carry — and [`decode_alpha`] now
//! *checks* that premise rather than assuming it, so artwork it is false for is refused rather
//! than silently flattened to one colour.
//!
//! Colour is the only axis on which nothing is lost. The reduction to [`MARK_TARGET_HEIGHT`] rows
//! (1767x474 down to 358x96 for the shipped artwork) throws away resolution, deliberately and
//! irreversibly: it is what keeps the embedded blob at 34 KiB rather than 3.3 MiB, sized in
//! [`MARK_TARGET_HEIGHT`]'s own note against how the mark is actually drawn.
//!
//! What the re-encoding buys is that `namir-ui` gains no dependency at all for this — it
//! `include_bytes!`es a mask and tints it, with no image decoder anywhere in either shipped
//! product. The decoder lives here, in dev tooling that is in neither product's dependency graph
//! (see this crate's `Cargo.toml`).
//!
//! # Why the whole reduction is integer-only
//!
//! [`check`] byte-compares a fresh render against the checked-in blob, so the blob has to be
//! reproducible on any machine a developer regenerates it from — not merely on the one runner
//! that happens to run the gate (`identity` runs in CI's single `ubuntu-latest`
//! `layering-and-params` job; it is not matrixed across platforms, and nothing here claims it is).
//! Any float in the path — a scale factor, a weight, a rounded average — risks a one-ULP
//! difference between two machines, which would make the checked-in artifact depend on *who* last
//! ran `--write` and turn the gate red for a reason nobody could act on. So [`target_width`] and
//! [`downsample_alpha`] use `u64` accumulation and integer division throughout, and there is no
//! `f32`/`f64` anywhere between reading the PNG's bytes and writing the blob's.

// trace-partial: NFR-DOC-040
// uncovered: NFR-DOC-040 — the "stating what it does" clause has no artifact: this check asserts
// uncovered: that `# Namir`, the two licence file names and the three build/run/test command lines
// uncovered: are present as substrings, and no static check can extend that to whether the prose
// uncovered: around them actually describes the product; closes M13
use std::path::Path;

/// Repository-relative path of the brand artwork every generated blob is derived from.
pub const MARK_SOURCE_PATH: &str = "images/namir.png";

/// Repository-relative path of the generated alpha blob `namir-ui` embeds.
pub const BLOB_PATH: &str = "crates/namir-ui/src/brand/namir_mark.alpha";

/// Height, in pixels, the mark is reduced to.
///
/// The blob is embedded once and never re-rendered per display, so it has to be dense enough for
/// the *densest* case it will be drawn in: a 2x HiDPI scale factor.
///
/// **This figure had roughly a 2x margin until M12's manual test, and now has none.** The mark was
/// drawn at one `TextStyle::Heading` row (~25 logical pixels, ~50 physical at 2x); executing the
/// test on Windows found it too small in both shells, and `namir-ui`'s
/// `brand::MARK_HEIGHT_IN_HEADINGS` doubled it. At ~50 logical pixels a 2x display asks for ~100
/// physical against the 96 stored here — ~1.04x magnification, effectively 1:1. That is the
/// sharpest this asset can be drawn, and it means **any further increase in the drawn size needs
/// this constant raised in the same change**, or the mark starts magnifying a mask with no more
/// detail in it.
///
/// The mark is still minified at a 1x scale factor (~2x, down from ~4-5x), and minification — not
/// magnification — is the direction in which a plain bilinear sample undersamples and aliases.
/// `namir-ui`'s `brand::render` therefore uploads the texture with mipmapping enabled; see
/// `MARK_TEXTURE_OPTIONS` there.
pub const MARK_TARGET_HEIGHT: u32 = 96;

/// The one colour `images/namir.png` is drawn in, and the colour `namir-ui`'s `brand::MARK_FILL`
/// re-tints the mask with. The two must agree; this is a second copy because `xtask` is in neither
/// product's dependency graph and so cannot import the other.
pub const MARK_FILL: [u8; 3] = [0xff, 0x66, 0x00];

/// Alpha at or above which a pixel's colour is held to [`MARK_FILL`] by [`decode_alpha`].
///
/// Below it a pixel contributes almost nothing to the rendered mark, and a PNG encoder is free to
/// put anything at all in the colour channels of a near-transparent texel: `images/namir.png` has
/// 294 texels stored as pure red, every one of them under alpha 16.
pub const FILL_ALPHA_FLOOR: u8 = 128;

/// Per-channel tolerance around [`MARK_FILL`] for a pixel at or above [`FILL_ALPHA_FLOOR`].
///
/// Measured against the shipped artwork (August 2026): of its 285,449 texels at alpha >= 128,
/// 283,559 are exactly `#ff6600` and the remaining 1,890 differ by **1** in green and by nothing
/// at all in red or blue. A tolerance of 8 therefore clears the real asset by 8x, while still
/// being far too tight for genuinely multi-coloured artwork, whose second fill would miss by tens
/// or hundreds at full alpha.
pub const FILL_TOLERANCE: u8 = 8;

/// Substrings `README.md` must contain (NFR-DOC-040): what the product is, how to build, run and
/// test it, and where its licence terms live.
const README_REQUIRED: [&str; 12] = [
    "# Namir",
    "images/namir.png",
    "## Building",
    "## Running",
    "## Testing",
    "## Licence",
    "cargo build --workspace",
    "cargo run -p namir-app",
    "cargo test --workspace",
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "TRADEMARK.md",
];

/// Substrings `TRADEMARK.md` must contain.
///
/// NFR-LIC-070 names an enumerated two-member set — the name "Namir" *and* the logo — and asks
/// that the terms on which each may be used be stated explicitly in the repository. Its `Verify:`
/// method is **S**, a static check, so executing that method as written means asserting that a
/// document at the repository root states those terms for **both** members. These five needles do
/// exactly that and nothing more: the heading that makes the document the one being looked for,
/// the code licence it is *distinguishing itself from*, the name member (`the name "Namir"`, the
/// literal phrase the name clause is written with), the logo member by where the assets live, and
/// the reservation stated explicitly rather than left to be inferred by omission.
// trace: NFR-LIC-070
const TRADEMARK_REQUIRED: [&str; 5] = [
    "# Trademark and brand assets",
    "MIT OR Apache-2.0",
    "the name \"Namir\"",
    "images/",
    "All rights reserved",
];

/// The remedy line every blob violation ends with, kept in one place so the check and its tests
/// cannot drift apart on the exact command a reader is told to run.
const BLOB_REMEDY: &str = "Run `cargo run -p xtask -- identity --write` to regenerate it.";

/// Width the mark reduces to at `target_h`, preserving aspect ratio.
///
/// Integer round-half-up (`(a * t + h / 2) / h`), never a float multiply: 1767x474 at a target
/// height of 96 is 357.87 px wide, which truncation would make 357 and rounding makes **358**.
/// Rounding is the better of the two here (it is the nearer of the two aspect ratios), and either
/// way the point is that the arithmetic is bit-identical on every platform.
pub fn target_width(src_w: u32, src_h: u32, target_h: u32) -> u32 {
    if src_h == 0 {
        return 0;
    }
    let (src_w, src_h, target_h) = (u64::from(src_w), u64::from(src_h), u64::from(target_h));
    ((src_w * target_h + src_h / 2) / src_h) as u32
}

/// Box/area-average downsample of an 8-bit single-channel image, integer arithmetic only.
///
/// Each destination pixel averages the half-open source rectangle it maps to, accumulating in
/// `u64` and dividing with round-half-up (`(sum + n / 2) / n`) so the mask keeps the source's
/// overall coverage rather than drifting darker the way truncation would. The rectangle is forced
/// to be non-empty (`max(start + 1)`, clamped to the source extent) so the function is total even
/// for an upscale, where it degenerates to nearest-neighbour rather than dividing by zero.
///
/// `src` is expected to be `src_w * src_h` bytes, row-major; a short slice yields zeros for the
/// missing samples rather than panicking.
pub fn downsample_alpha(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((dst_w as usize) * (dst_h as usize));
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return out;
    }

    for y in 0..dst_h {
        let (y0, y1) = span(y, dst_h, src_h);
        for x in 0..dst_w {
            let (x0, x1) = span(x, dst_w, src_w);
            let mut sum: u64 = 0;
            let mut count: u64 = 0;
            for sy in y0..y1 {
                let row = (sy as usize) * (src_w as usize);
                for sx in x0..x1 {
                    sum += u64::from(src.get(row + sx as usize).copied().unwrap_or(0));
                    count += 1;
                }
            }
            out.push(((sum + count / 2) / count) as u8);
        }
    }

    out
}

/// The half-open source span `[start, end)` destination index `i` of `dst_len` maps onto a source
/// extent of `src_len`. Always non-empty and always within `0..src_len`.
fn span(i: u32, dst_len: u32, src_len: u32) -> (u32, u32) {
    let (i, dst_len, src_len) = (u64::from(i), u64::from(dst_len), u64::from(src_len));
    let start = (i * src_len / dst_len) as u32;
    let end = ((i + 1) * src_len / dst_len) as u32;
    let src_len = src_len as u32;
    let start = start.min(src_len.saturating_sub(1));
    (start, end.max(start + 1).min(src_len))
}

/// Decodes an 8-bit RGBA PNG, verifies it is a single-fill image, and returns
/// `(width, height, alpha_channel)`.
///
/// Deliberately strict about the input on two counts, both of them the premise that discarding the
/// colour channels is colour-lossless:
///
/// * the colour type must be 8-bit RGBA (PNG colour type 6). `images/namir.png` is, and a
///   different colour type would mean either the artwork changed shape or the wrong file was
///   passed;
/// * every pixel at or above [`FILL_ALPHA_FLOOR`] alpha must be within [`FILL_TOLERANCE`] of
///   [`MARK_FILL`] on every channel. Without this, multi-coloured artwork would decode without a
///   word of complaint and then render as a flat orange blob, because `namir-ui` re-tints the mask
///   with one constant — a silent visual regression that no byte comparison downstream could
///   catch, since the blob would agree with the render that produced it.
///
/// Both are an error naming what was found rather than a silent reinterpretation of the bytes.
pub fn decode_alpha(png_bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let decoder = png::Decoder::new(png_bytes);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("could not read the PNG header: {e}"))?;

    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("could not decode the PNG image data: {e}"))?;

    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(format!(
            "expected an 8-bit RGBA PNG (colour type 6), found {:?} at {:?} -- the alpha-mask \
             encoding is only colour-lossless for artwork that is a single fill on a \
             transparent background",
            info.color_type, info.bit_depth
        ));
    }

    let frame = &buf[..info.buffer_size()];
    if let Some((i, px)) = frame
        .chunks_exact(4)
        .enumerate()
        .find(|(_, px)| px[3] >= FILL_ALPHA_FLOOR && !is_mark_fill(px))
    {
        let i = i as u32;
        let (x, y) = (i % info.width.max(1), i / info.width.max(1));
        return Err(format!(
            "pixel ({x}, {y}) is rgb({}, {}, {}) at alpha {}, further than {FILL_TOLERANCE} per \
             channel from the brand fill rgb({}, {}, {}) — the alpha-mask encoding is only \
             colour-lossless for artwork that is a single fill on a transparent background",
            px[0], px[1], px[2], px[3], MARK_FILL[0], MARK_FILL[1], MARK_FILL[2]
        ));
    }

    let alpha = frame.iter().skip(3).step_by(4).copied().collect();
    Ok((info.width, info.height, alpha))
}

/// Whether an RGBA pixel's colour is within [`FILL_TOLERANCE`] of [`MARK_FILL`] on every channel.
fn is_mark_fill(px: &[u8]) -> bool {
    px.iter()
        .zip(MARK_FILL)
        .all(|(&c, fill)| c.abs_diff(fill) <= FILL_TOLERANCE)
}

/// Full generation path: decode `png_bytes`, reduce its alpha channel to `target_h` rows, and
/// frame the result as the on-disk blob — a `u32` width then a `u32` height, both little-endian,
/// followed by `width * height` alpha bytes, row-major.
pub fn render_blob(png_bytes: &[u8], target_h: u32) -> Result<Vec<u8>, String> {
    let (src_w, src_h, alpha) = decode_alpha(png_bytes)?;
    let dst_h = target_h.min(src_h).max(1);
    let dst_w = target_width(src_w, src_h, dst_h).max(1);
    let pixels = downsample_alpha(&alpha, src_w, src_h, dst_w, dst_h);

    let mut out = Vec::with_capacity(8 + pixels.len());
    out.extend_from_slice(&dst_w.to_le_bytes());
    out.extend_from_slice(&dst_h.to_le_bytes());
    out.extend_from_slice(&pixels);
    Ok(out)
}

/// Every required substring of `required` that `text` does not contain, as one violation line
/// each. Pure and in-memory so the whole document half of this gate is unit-testable without the
/// documents themselves existing.
pub fn missing_substrings(label: &str, text: &str, required: &[&str]) -> Vec<String> {
    required
        .iter()
        .filter(|needle| !text.contains(**needle))
        .map(|needle| format!("{label} does not contain the required text `{needle}`"))
        .collect()
}

/// Reads `<repo_root>/<label>` and applies [`missing_substrings`]; a missing file is one
/// violation, not a per-substring cascade.
fn check_document(repo_root: &Path, label: &str, required: &[&str]) -> Vec<String> {
    match std::fs::read_to_string(repo_root.join(label)) {
        Ok(text) => missing_substrings(label, &text, required),
        Err(e) => vec![format!(
            "{label} does not exist at the repository root, or could not be read ({e})"
        )],
    }
}

/// Compares the checked-in blob against a fresh render of `images/namir.png`. Split out from
/// [`check`] so a test can drive it against a synthetic root.
fn check_blob(repo_root: &Path, expected: &[u8]) -> Vec<String> {
    let blob_path = repo_root.join(BLOB_PATH);
    match std::fs::read(&blob_path) {
        Ok(actual) if actual == expected => Vec::new(),
        Ok(_) => vec![format!(
            "{} is stale -- it does not match a fresh render of {}. {BLOB_REMEDY}",
            blob_path.display(),
            MARK_SOURCE_PATH
        )],
        Err(e) => vec![format!(
            "{} could not be read ({e}). {BLOB_REMEDY}",
            blob_path.display()
        )],
    }
}

/// Every identity violation under `repo_root`, empty meaning the gate passes.
///
/// `Err` is reserved for an input this check cannot evaluate at all — an unreadable or
/// non-RGBA `images/namir.png` — as distinct from a violation, which is a finding about the
/// repository that the caller prints and fails on.
pub fn check(repo_root: &Path) -> Result<Vec<String>, String> {
    let png_path = repo_root.join(MARK_SOURCE_PATH);
    let png_bytes = std::fs::read(&png_path)
        .map_err(|e| format!("failed to read {}: {e}", png_path.display()))?;
    let expected = render_blob(&png_bytes, MARK_TARGET_HEIGHT)
        .map_err(|e| format!("{}: {e}", png_path.display()))?;

    let mut violations = check_blob(repo_root, &expected);
    violations.extend(check_document(repo_root, "README.md", &README_REQUIRED));
    violations.extend(check_document(
        repo_root,
        "TRADEMARK.md",
        &TRADEMARK_REQUIRED,
    ));
    Ok(violations)
}

/// `--write`: regenerates the brand blob from `images/namir.png`. Returns a human-readable status
/// line for CI logs, the same contract `params_lock`/`attribution` use.
///
/// Only the blob is generated — the two Markdown documents are prose, written by a human, and this
/// subcommand checks them rather than producing them.
pub fn write_blob(repo_root: &Path) -> Result<String, String> {
    let png_path = repo_root.join(MARK_SOURCE_PATH);
    let png_bytes = std::fs::read(&png_path)
        .map_err(|e| format!("failed to read {}: {e}", png_path.display()))?;
    let blob = render_blob(&png_bytes, MARK_TARGET_HEIGHT)
        .map_err(|e| format!("{}: {e}", png_path.display()))?;

    let blob_path = repo_root.join(BLOB_PATH);
    if let Some(parent) = blob_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    std::fs::write(&blob_path, &blob)
        .map_err(|e| format!("failed to write {}: {e}", blob_path.display()))?;

    let width = u32::from_le_bytes(blob[0..4].try_into().expect("the header is 8 bytes"));
    let height = u32::from_le_bytes(blob[4..8].try_into().expect("the header is 8 bytes"));
    Ok(format!(
        "wrote {} ({width}x{height} alpha mask, {} bytes)",
        blob_path.display(),
        blob.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_width_rounds_rather_than_truncating() {
        // The shipped case: 1767x474 at 96 rows is 357.87 px, which truncation makes 357.
        assert_eq!(target_width(1767, 474, 96), 358);
        // An exact ratio is unaffected by the rounding term.
        assert_eq!(target_width(400, 200, 100), 200);
        // Degenerate input is total rather than a division by zero.
        assert_eq!(target_width(100, 0, 96), 0);
    }

    #[test]
    fn downsample_averages_each_source_box() {
        // 4x2 -> 2x1: each destination pixel averages a 2x2 box.
        let src = [0, 10, 100, 200, 20, 30, 40, 60];
        let out = downsample_alpha(&src, 4, 2, 2, 1);
        assert_eq!(out, vec![15, 100]);
    }

    #[test]
    fn downsample_of_a_uniform_image_is_uniform() {
        let src = vec![137u8; 40 * 24];
        let out = downsample_alpha(&src, 40, 24, 10, 6);
        assert_eq!(out.len(), 60);
        assert!(out.iter().all(|&a| a == 137));
    }

    #[test]
    fn downsample_is_deterministic_and_correctly_sized() {
        let src: Vec<u8> = (0..(37 * 19)).map(|i| (i % 251) as u8).collect();
        let a = downsample_alpha(&src, 37, 19, 8, 4);
        let b = downsample_alpha(&src, 37, 19, 8, 4);
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn downsample_tolerates_degenerate_extents() {
        assert!(downsample_alpha(&[1, 2, 3], 3, 1, 0, 1).is_empty());
        assert!(downsample_alpha(&[], 0, 0, 4, 4).is_empty());
        // Upscaling degenerates to nearest-neighbour rather than dividing by zero.
        assert_eq!(
            downsample_alpha(&[10, 20], 2, 1, 4, 1),
            vec![10, 10, 20, 20]
        );
    }

    #[test]
    fn missing_substrings_reports_one_line_per_absent_needle() {
        let violations = missing_substrings(
            "README.md",
            "# Namir\n\n## Building\n",
            &["# Namir", "## Building", "## Licence", "LICENSE-MIT"],
        );
        assert_eq!(violations.len(), 2);
        assert!(violations[0].contains("`## Licence`"));
        assert!(violations[1].contains("`LICENSE-MIT`"));
        assert!(violations.iter().all(|v| v.starts_with("README.md")));
    }

    #[test]
    fn missing_substrings_is_empty_for_a_conforming_document() {
        let readme = "# Namir\n![](images/namir.png)\n## Building\n`cargo build --workspace`\n\
             ## Running\n`cargo run -p namir-app`\n## Testing\n`cargo test --workspace`\n\
             ## Licence\nLICENSE-MIT / LICENSE-APACHE, see TRADEMARK.md\n";
        assert!(missing_substrings("README.md", readme, &README_REQUIRED).is_empty());
    }

    #[test]
    fn trademark_needles_are_satisfied_by_a_conforming_document() {
        let trademark = "# Trademark and brand assets\n\nThe code is MIT OR Apache-2.0. \
             That licence does not extend to the name \"Namir\", nor to the marks under images/. \
             All rights reserved.\n";
        assert!(missing_substrings("TRADEMARK.md", trademark, &TRADEMARK_REQUIRED).is_empty());
    }

    /// NFR-LIC-070 enumerates two members, the name and the logo, and a document covering only one
    /// of them does not satisfy it. The needle set is what makes that a build error rather than a
    /// judgement call, so it is asserted here directly.
    #[test]
    fn a_trademark_document_silent_on_the_name_is_a_violation() {
        let logo_only = "# Trademark and brand assets\n\nThe code is MIT OR Apache-2.0. \
             The marks under images/ are not. All rights reserved.\n";
        let violations = missing_substrings("TRADEMARK.md", logo_only, &TRADEMARK_REQUIRED);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("the name"), "{}", violations[0]);
    }

    #[test]
    fn trademark_check_names_every_absent_needle() {
        let violations =
            missing_substrings("TRADEMARK.md", "# Nothing here\n", &TRADEMARK_REQUIRED);
        assert_eq!(violations.len(), TRADEMARK_REQUIRED.len());
    }

    #[test]
    fn a_missing_document_is_one_violation_not_a_cascade() {
        let dir = std::env::temp_dir().join(format!("xtask-identity-doc-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let violations = check_document(&dir, "README.md", &README_REQUIRED);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("does not exist at the repository root"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn blob_check_names_the_write_command_when_stale_or_absent() {
        let dir = std::env::temp_dir().join(format!("xtask-identity-blob-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("crates/namir-ui/src/brand")).unwrap();

        let absent = check_blob(&dir, b"expected");
        assert_eq!(absent.len(), 1);
        assert!(absent[0].contains("cargo run -p xtask -- identity --write"));

        std::fs::write(dir.join(BLOB_PATH), b"something else").unwrap();
        let stale = check_blob(&dir, b"expected");
        assert_eq!(stale.len(), 1);
        assert!(stale[0].contains("stale"));
        assert!(stale[0].contains("cargo run -p xtask -- identity --write"));

        std::fs::write(dir.join(BLOB_PATH), b"expected").unwrap();
        assert!(check_blob(&dir, b"expected").is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The blob's framing, exercised end to end against a synthetic 4x2 RGBA PNG rather than the
    /// real artwork, so the test says nothing about what the artwork happens to look like today.
    #[test]
    fn render_blob_frames_the_mask_with_a_little_endian_header() {
        let png = synthetic_rgba_png(4, 2, |x, y| [0xff, 0x66, 0x00, (x * 40 + y * 7) as u8]);
        let blob = render_blob(&png, 1).unwrap();

        assert_eq!(&blob[0..4], &2u32.to_le_bytes());
        assert_eq!(&blob[4..8], &1u32.to_le_bytes());
        assert_eq!(blob.len(), 8 + 2);
    }

    #[test]
    fn render_blob_preserves_the_source_aspect_ratio() {
        let png = synthetic_rgba_png(1767, 474, |_, _| [0xff, 0x66, 0x00, 0x80]);
        let blob = render_blob(&png, MARK_TARGET_HEIGHT).unwrap();

        assert_eq!(&blob[0..4], &358u32.to_le_bytes());
        assert_eq!(&blob[4..8], &96u32.to_le_bytes());
        assert_eq!(blob.len(), 8 + 358 * 96);
        assert!(blob[8..].iter().all(|&a| a == 0x80));
    }

    #[test]
    fn render_blob_is_byte_identical_across_repeated_renders() {
        let png = synthetic_rgba_png(97, 31, |x, y| {
            [0xff, 0x66, 0x00, ((x * 13 + y * 29) % 256) as u8]
        });
        assert_eq!(
            render_blob(&png, 12).unwrap(),
            render_blob(&png, 12).unwrap()
        );
    }

    #[test]
    fn a_non_rgba_png_is_refused_by_name() {
        let png = synthetic_grayscale_png(4, 2);
        let err = render_blob(&png, 1).unwrap_err();
        assert!(err.contains("8-bit RGBA"), "{err}");
    }

    /// Artwork the alpha-mask encoding is *not* colour-lossless for must be refused, not reduced
    /// to a mask and re-tinted into a flat orange blob.
    #[test]
    fn a_multi_coloured_png_is_refused_by_name() {
        let png = synthetic_rgba_png(4, 2, |x, _| {
            if x < 2 {
                [0xff, 0x66, 0x00, 0xff]
            } else {
                [0x00, 0x66, 0xff, 0xff]
            }
        });
        let err = render_blob(&png, 1).unwrap_err();
        assert!(err.contains("colour-lossless"), "{err}");
        assert!(
            err.contains("(2, 0)"),
            "the offending pixel is not named: {err}"
        );
    }

    /// The tolerance exists because real artwork is not bit-exact (see [`FILL_TOLERANCE`]): a
    /// texel a shade off the fill must still pass, and a near-transparent texel's colour channels
    /// are not looked at at all, because an encoder is free to put anything in them.
    #[test]
    fn near_fill_and_near_transparent_pixels_are_tolerated() {
        // Off by 1 in green at full alpha -- exactly the shipped artwork's worst case.
        let nearly = synthetic_rgba_png(4, 2, |_, _| [0xff, 0x67, 0x00, 0xff]);
        assert!(render_blob(&nearly, 1).is_ok());

        // Pure red, but below FILL_ALPHA_FLOOR: not looked at, as in the real PNG.
        let ghost = synthetic_rgba_png(4, 2, |_, _| [0xff, 0x00, 0x00, FILL_ALPHA_FLOOR - 1]);
        assert!(render_blob(&ghost, 1).is_ok());

        // The same colour at the floor is refused.
        let visible = synthetic_rgba_png(4, 2, |_, _| [0xff, 0x00, 0x00, FILL_ALPHA_FLOOR]);
        assert!(render_blob(&visible, 1).is_err());
    }

    /// The gate is only as good as its premise, so assert the premise against the real artwork
    /// rather than only against synthetic inputs: `images/namir.png` itself must pass
    /// [`decode_alpha`]'s single-fill check.
    #[test]
    fn the_shipped_artwork_is_a_single_fill() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ has a parent");
        let png = std::fs::read(root.join(MARK_SOURCE_PATH)).expect("the artwork is checked in");
        let (w, h, alpha) = decode_alpha(&png).expect("the shipped artwork is a single fill");
        assert_eq!((w, h), (1767, 474));
        assert_eq!(alpha.len(), (w * h) as usize);
    }

    fn synthetic_rgba_png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&pixel(x, y));
            }
        }
        encode_png(width, height, png::ColorType::Rgba, &data)
    }

    fn synthetic_grayscale_png(width: u32, height: u32) -> Vec<u8> {
        let data = vec![0x40u8; (width * height) as usize];
        encode_png(width, height, png::ColorType::Grayscale, &data)
    }

    fn encode_png(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(data).unwrap();
        }
        out
    }
}
