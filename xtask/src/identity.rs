//! M12's product-identity gate: one subcommand covering the three artifacts that carry Namir's
//! identity in the repository — the brand mark `namir-ui` renders, the `README.md` a reader of
//! this repository lands on, and the `TRADEMARK.md` that separates the brand from the code
//! licence.
//!
//! Same generate-and-diff shape as `params_lock.rs` and `attribution.rs`, with one difference
//! worth stating: the generated artifacts are **binary**, not text. `images/namir.png` is the
//! source of truth for both; [`render_blob`] reduces it to an 8-bit alpha mask and [`render_ico`]
//! reduces it to a Windows icon; [`check`] byte-compares each against its checked-in file
//! (`crates/namir-ui/src/brand/namir_mark.alpha` and `images/namir.ico`), or [`write_generated`]
//! regenerates both with `--write`. The other two checks are pure substring assertions over the two
//! Markdown documents, which is all a `Verify: S` static check can be for a prose artifact.
//!
//! # The second artifact: `images/namir.ico` (M13)
//!
//! FR-UI-110's executable-icon clause needs a Windows `.ico`, and `02-architecture.md` **D-17.3**
//! rules out the usual route to one — a `winresource`/`embed-resource` build script in a shipped
//! crate — so the icon is embedded by M13's packaging pipeline from a file that has to exist
//! independently of any `cargo build`. Two shapes were available for that file.
//!
//! * **A checked-in binary `.ico` nobody can regenerate.** Rejected. It would be the only artwork
//!   in the repository with no stated derivation from `images/namir.png`, so a change to the source
//!   artwork would leave it silently stale — the exact failure the blob's freshness gate exists to
//!   prevent, reintroduced beside it.
//! * **Generated here, freshness-gated, integer-only.** Taken, because it is the shape this project
//!   already chose for the same problem one milestone earlier and every argument below the fold in
//!   this module applies unchanged: one source of truth, byte-comparable output, and arithmetic
//!   that cannot depend on which machine ran `--write`.
//!
//! Two things the icon needs that the blob did not.
//!
//! **A square crop.** `images/namir.png` is a 3.73:1 wordmark with the leopard-head mark at its
//! right-hand end; a Windows icon is square, and letterboxing the whole mark into a square would
//! leave four rows of legible pixels at 16x16. [`icon_crop`] therefore takes the **rightmost
//! `height x height` square**, which is the head. That is a claim about the artwork's layout rather
//! than about icons in general, so it is *checked* rather than assumed: [`check_icon_gutter`]
//! refuses artwork whose wordmark reaches into the crop, in the same spirit as [`decode_alpha`]'s
//! single-fill check.
//!
//! **An uncompressed encoding.** Every image in the file is a plain 32-bit BGRA DIB
//! ([`ico_image`]). Windows Vista and later would also accept a PNG-compressed 256x256 entry, which
//! would take the file from ~279 KiB to ~10 KiB — and would make a checked-in, byte-compared
//! artifact depend on a third-party deflate implementation's internal heuristics, which is the one
//! property the section below says this generator must not have. The size is paid once, in a
//! repository that already carries the 84 KiB source PNG, and it buys an encoder whose output is a
//! function of the source bytes and this file's own integer arithmetic and of nothing else.
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
//! `f32`/`f64` anywhere between reading the PNG's bytes and writing the blob's — nor between
//! reading them and writing the icon's, which shares [`downsample_alpha`] and adds only a crop and
//! a byte-level container.

// trace-partial: NFR-DOC-040
// uncovered: NFR-DOC-040 — the "stating what it does" clause has no artifact: this check asserts
// uncovered: that `# Namir`, the two licence file names and the three build/run/test command lines
// uncovered: are present as substrings, and no static check can extend that to whether the prose
// uncovered: around them actually describes the product; closes M8
use std::path::{Path, PathBuf};

/// Repository-relative path of the brand artwork every generated blob is derived from.
pub const MARK_SOURCE_PATH: &str = "images/namir.png";

/// Repository-relative path of the generated alpha blob `namir-ui` embeds.
pub const BLOB_PATH: &str = "crates/namir-ui/src/brand/namir_mark.alpha";

/// Repository-relative path of the generated Windows icon the packaging pipeline embeds.
///
/// Under `images/` rather than under `packaging/windows/` for a reason that outlives the packaging
/// lane: it is a **brand asset**, and `TRADEMARK.md` reserves rights over the marks "under
/// `images/`" — the literal needle `TRADEMARK_REQUIRED` asserts. An icon carrying the leopard head
/// but living outside that directory would sit outside the sentence NFR-LIC-070 is met by.
pub const ICO_PATH: &str = "images/namir.ico";

/// Square edge lengths, in pixels, the generated icon carries.
///
/// Microsoft's recommended minimum set for an application icon. 16 is the title bar, the taskbar
/// and Explorer's small-icon views; 32 is the taskbar at 2x and Alt-Tab; 48 is Explorer's
/// medium-icon default; 256 is its extra-large and "jumbo" views and the Inno Setup wizard's own
/// header. The intermediate sizes Windows also asks for (24, 64, 96, 128) are scaled by the shell
/// from the neighbours above, which is cheaper than storing them: an uncompressed 128 entry alone
/// would add 66 KiB for a size no shell surface asks for by default.
pub const ICO_SIZES: [u32; 4] = [16, 32, 48, 256];

/// Largest share of the source's rows, as a percentage, that the column immediately left of
/// [`icon_crop`]'s square may carry ink in.
///
/// This is the icon's counterpart to [`FILL_TOLERANCE`]: the crop rule is only correct if the
/// artwork really is a wordmark followed by a square mark, and this is what turns that premise into
/// a check. Measured against the shipped artwork (August 2026): the column at the crop's left edge
/// carries **9** of 474 rows at or above [`FILL_ALPHA_FLOOR`] — 1.9 %, the tapering tip of the
/// leopard's leftmost whisker — while the stem of the wordmark's `r`, seventeen columns further
/// left, carries **77**, or 16.2 %. A 5 % limit therefore clears the real asset by 2.6x and would
/// refuse a layout in which the wordmark reached the crop by better than 3x.
pub const ICON_GUTTER_MAX_COVERAGE_PERCENT: u32 = 5;

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

/// NFR-LIC-010's "licence files present" check (M14): the two files the dual licence is offered
/// under, each with a substring that identifies which licence it actually contains and the
/// copyright holder the requirement names.
///
/// # Why substrings rather than existence alone
///
/// The requirement is "Namir shall be published under `MIT OR Apache-2.0`, at the recipient's
/// option, with the copyright held by Erwan Patrick Legrand", and *presence of a file called
/// `LICENSE-MIT`* is not that. An empty file, a truncated one, or one carrying the wrong licence
/// text would satisfy an existence test while leaving the recipient without the option the
/// requirement grants them. Two needles per file — the licence's own title line, and the copyright
/// line naming the holder — cost nothing and assert what is actually promised.
///
/// `LICENSE-APACHE` carries the Apache-2.0 text verbatim, whose appendix is the place the holder is
/// named, so its holder needle is the same string in a different position; both files are checked
/// the same way rather than one being special-cased.
///
/// This closes the half of the method the FRS's `*Consequence (added M14)*` note books rather than
/// accepts. The SPDX-header half stays open and is NFR-LIC-060's (a Should's) work — see that note.
const LICENCE_FILES: [(&str, [&str; 2]); 2] = [
    ("LICENSE-MIT", ["MIT License", "Erwan Patrick Legrand"]),
    (
        "LICENSE-APACHE",
        ["Apache License", "Erwan Patrick Legrand"],
    ),
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

/// The remedy line every generated-artifact violation ends with, kept in one place so the check and
/// its tests cannot drift apart on the exact command a reader is told to run.
const WRITE_REMEDY: &str = "Run `cargo run -p xtask -- identity --write` to regenerate it.";

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

/// The source rectangle the icon is cut from: `(x, y, side)`, the **rightmost** square the source
/// can hold.
///
/// The rule is one line because the artwork is laid out for it — the leopard head occupies the
/// right-hand end of the wordmark and is very nearly square — and because anything more elaborate
/// (a connected-component search, a hand-entered rectangle) would either be a heuristic that fails
/// silently or a second place the artwork's geometry is written down. It is verified rather than
/// trusted; see [`check_icon_gutter`].
///
/// Total for any input, including a source that is square or taller than it is wide: `side` is the
/// smaller extent, so the crop is always inside the image, and a tall source is cut from its
/// vertical centre.
pub fn icon_crop(src_w: u32, src_h: u32) -> (u32, u32, u32) {
    let side = src_w.min(src_h);
    (src_w - side, (src_h - side) / 2, side)
}

/// The `w x h` sub-rectangle of a single-channel image at `(x0, y0)`, row-major.
///
/// `w` is clamped to what remains of the row so that an over-wide request yields a narrower crop
/// rather than silently wrapping into the next row's pixels; samples past the end of `src` read as
/// zero, the same convention [`downsample_alpha`] uses.
pub fn crop_alpha(src: &[u8], src_w: u32, x0: u32, y0: u32, w: u32, h: u32) -> Vec<u8> {
    let w = w.min(src_w.saturating_sub(x0));
    let mut out = Vec::with_capacity((w as usize) * (h as usize));
    for y in y0..y0.saturating_add(h) {
        let row = (y as usize) * (src_w as usize);
        for x in x0..x0 + w {
            out.push(src.get(row + x as usize).copied().unwrap_or(0));
        }
    }
    out
}

/// How many of a column's rows are at or above [`FILL_ALPHA_FLOOR`] — i.e. how much of it a reader
/// would see as ink rather than as background.
fn column_ink(alpha: &[u8], src_w: u32, src_h: u32, x: u32) -> u32 {
    (0..src_h)
        .filter(|y| {
            alpha
                .get((*y as usize) * (src_w as usize) + x as usize)
                .copied()
                .unwrap_or(0)
                >= FILL_ALPHA_FLOOR
        })
        .count() as u32
}

/// Verifies [`icon_crop`]'s premise: that its left edge falls in the gap between the wordmark and
/// the square mark, not through a letter.
///
/// Without this, artwork whose wordmark ran further right — or artwork that was *only* a wordmark —
/// would generate an icon showing a sliver of a letter, at every size, with nothing anywhere
/// reporting it. A byte comparison downstream cannot catch that: the checked-in file would agree
/// perfectly with the render that produced it. So the premise is refused here rather than
/// reinterpreted, exactly as [`decode_alpha`] refuses multi-coloured artwork.
///
/// Vacuously true when the crop starts at column 0, which is every source at least as tall as it is
/// wide: there is no column to its left to be a gutter.
pub fn check_icon_gutter(alpha: &[u8], src_w: u32, src_h: u32, x0: u32) -> Result<(), String> {
    if x0 == 0 {
        return Ok(());
    }
    let gutter = x0 - 1;
    let ink = column_ink(alpha, src_w, src_h, gutter);
    let limit = src_h * ICON_GUTTER_MAX_COVERAGE_PERCENT / 100;
    if ink > limit {
        return Err(format!(
            "column {gutter}, immediately left of the {side}x{side} icon crop at x={x0}, carries \
             ink in {ink} of {src_h} rows, above the {ICON_GUTTER_MAX_COVERAGE_PERCENT}% limit of \
             {limit} — the icon is cut from the rightmost square of the artwork on the premise that \
             the square mark sits at its right-hand end, and this artwork's wordmark reaches into \
             that square",
            side = src_w - x0
        ));
    }
    Ok(())
}

/// Full icon-generation path: decode `png_bytes`, cut the square mark out of it, reduce that to
/// each of [`ICO_SIZES`], and frame the results as a Windows `.ico` file.
pub fn render_ico(png_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let (src_w, src_h, alpha) = decode_alpha(png_bytes)?;
    let (x0, y0, side) = icon_crop(src_w, src_h);
    check_icon_gutter(&alpha, src_w, src_h, x0)?;
    let mark = crop_alpha(&alpha, src_w, x0, y0, side, side);

    let images: Vec<(u32, Vec<u8>)> = ICO_SIZES
        .iter()
        .map(|&size| {
            let scaled = downsample_alpha(&mark, side, side, size, size);
            (size, ico_image(size, &scaled))
        })
        .collect();
    Ok(assemble_ico(&images))
}

/// One `ICONDIRENTRY` dimension byte. The format stores the edge length in a single byte, so 256 —
/// the largest size Windows reads — is spelled `0`, and nothing larger can be expressed at all.
fn ico_entry_dimension(size: u32) -> u8 {
    if size >= 256 { 0 } else { size as u8 }
}

/// One icon image: a `BITMAPINFOHEADER` followed by a 32-bit BGRA colour bitmap and a 1-bit AND
/// mask, which is what an `.ico` entry is when it is not a PNG.
///
/// Three details of the format that are easy to get wrong and are therefore spelled out:
///
/// * `biHeight` is **twice** the image height, because the header describes the colour bitmap and
///   the AND mask as one stacked image;
/// * both bitmaps are stored **bottom-up**, so the rows are emitted in reverse;
/// * the colour bytes are **straight**, not premultiplied, alpha — the convention `.ico` readers
///   apply, and the only one that keeps a `#ff6600` edge pixel orange rather than darkening it
///   toward black as its alpha falls.
///
/// The AND mask is redundant for a 32-bit image on any Windows that reads the alpha channel, and is
/// still written correctly (1 = leave the background alone) rather than zero-filled, so that a
/// legacy path taking it at its word gets the silhouette instead of a black square.
fn ico_image(size: u32, mask: &[u8]) -> Vec<u8> {
    let side = size as usize;
    let and_row = side.div_ceil(32) * 4;
    let xor_len = side * side * 4;
    let and_len = and_row * side;

    let mut out = Vec::with_capacity(40 + xor_len + and_len);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(size as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&((size as i32) * 2).to_le_bytes()); // biHeight: colour + mask
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression: BI_RGB
    out.extend_from_slice(&((xor_len + and_len) as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0u32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    for y in (0..side).rev() {
        for x in 0..side {
            let alpha = mask.get(y * side + x).copied().unwrap_or(0);
            out.push(MARK_FILL[2]);
            out.push(MARK_FILL[1]);
            out.push(MARK_FILL[0]);
            out.push(alpha);
        }
    }

    for y in (0..side).rev() {
        let mut row = vec![0u8; and_row];
        for x in 0..side {
            if mask.get(y * side + x).copied().unwrap_or(0) < FILL_ALPHA_FLOOR {
                row[x / 8] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&row);
    }

    out
}

/// The `.ico` container: a six-byte `ICONDIR`, one sixteen-byte `ICONDIRENTRY` per image, then the
/// images themselves in the same order.
fn assemble_ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut directory = Vec::with_capacity(6 + 16 * images.len());
    directory.extend_from_slice(&0u16.to_le_bytes()); // reserved
    directory.extend_from_slice(&1u16.to_le_bytes()); // type 1 = icon (2 would be a cursor)
    directory.extend_from_slice(&(images.len() as u16).to_le_bytes());

    let mut offset = 6 + 16 * images.len();
    let mut body = Vec::new();
    for (size, image) in images {
        directory.push(ico_entry_dimension(*size));
        directory.push(ico_entry_dimension(*size));
        directory.push(0); // palette size: 0 for a direct-colour image
        directory.push(0); // reserved
        directory.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        directory.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        directory.extend_from_slice(&(image.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
        body.extend_from_slice(image);
    }

    directory.extend_from_slice(&body);
    directory
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
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .find(|(_, px)| px[3] >= FILL_ALPHA_FLOOR && !is_mark_fill(px.as_slice()))
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

/// Compares one checked-in generated artifact against a fresh render of `images/namir.png`. Split
/// out from [`check`] so a test can drive it against a synthetic root.
fn check_generated(repo_root: &Path, rel_path: &str, expected: &[u8]) -> Vec<String> {
    let path = repo_root.join(rel_path);
    match std::fs::read(&path) {
        Ok(actual) if actual == expected => Vec::new(),
        Ok(_) => vec![format!(
            "{} is stale -- it does not match a fresh render of {}. {WRITE_REMEDY}",
            path.display(),
            MARK_SOURCE_PATH
        )],
        Err(e) => vec![format!(
            "{} could not be read ({e}). {WRITE_REMEDY}",
            path.display()
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
    let expected_blob = render_blob(&png_bytes, MARK_TARGET_HEIGHT)
        .map_err(|e| format!("{}: {e}", png_path.display()))?;
    let expected_ico =
        render_ico(&png_bytes).map_err(|e| format!("{}: {e}", png_path.display()))?;

    let mut violations = check_generated(repo_root, BLOB_PATH, &expected_blob);
    violations.extend(check_generated(repo_root, ICO_PATH, &expected_ico));
    violations.extend(check_document(repo_root, "README.md", &README_REQUIRED));
    violations.extend(check_document(
        repo_root,
        "TRADEMARK.md",
        &TRADEMARK_REQUIRED,
    ));
    for (label, required) in LICENCE_FILES {
        violations.extend(check_document(repo_root, label, &required));
    }
    Ok(violations)
}

/// `--write`: regenerates both binary artifacts from `images/namir.png`. Returns one
/// human-readable status line per artifact for CI logs — a list rather than the single line
/// `params_lock`/`attribution` return, for the same reason [`check`] returns one: a reader should
/// see what each artifact did, not a summary that hides one behind the other.
///
/// Only these two are generated — the two Markdown documents are prose, written by a human, and
/// this subcommand checks them rather than producing them.
pub fn write_generated(repo_root: &Path) -> Result<Vec<String>, String> {
    let png_path = repo_root.join(MARK_SOURCE_PATH);
    let png_bytes = std::fs::read(&png_path)
        .map_err(|e| format!("failed to read {}: {e}", png_path.display()))?;

    let blob = render_blob(&png_bytes, MARK_TARGET_HEIGHT)
        .map_err(|e| format!("{}: {e}", png_path.display()))?;
    let blob_path = write_artifact(repo_root, BLOB_PATH, &blob)?;
    let width = u32::from_le_bytes(blob[0..4].try_into().expect("the header is 8 bytes"));
    let height = u32::from_le_bytes(blob[4..8].try_into().expect("the header is 8 bytes"));

    let ico = render_ico(&png_bytes).map_err(|e| format!("{}: {e}", png_path.display()))?;
    let ico_path = write_artifact(repo_root, ICO_PATH, &ico)?;
    let sizes: Vec<String> = ICO_SIZES.iter().map(|s| format!("{s}x{s}")).collect();

    Ok(vec![
        format!(
            "wrote {} ({width}x{height} alpha mask, {} bytes)",
            blob_path.display(),
            blob.len()
        ),
        format!(
            "wrote {} ({}, 32-bit uncompressed, {} bytes)",
            ico_path.display(),
            sizes.join(" + "),
            ico.len()
        ),
    ])
}

/// Writes one generated artifact, creating its directory if need be, and returns the path written
/// so the caller can name it.
fn write_artifact(repo_root: &Path, rel_path: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let path = repo_root.join(rel_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, bytes).map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    Ok(path)
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

    // --- NFR-LIC-010: licence files present (M14) ---------------------------------------------

    /// The real repository root, which is what `xtask identity` runs against.
    fn real_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask's manifest dir always has a parent")
            .to_path_buf()
    }

    /// NFR-LIC-010's "licence files present" clause, against the real tree. Deleting `LICENSE-MIT`
    /// or `LICENSE-APACHE` -- or emptying one, or replacing its text with the other licence's --
    /// went unnoticed by every gate in this repository until M14.
    // trace-partial: NFR-LIC-010
    // uncovered: NFR-LIC-010 — the method's SPDX-header check has no artifact: only two tracked
    // uncovered: files under crates/ carry a header, and making the check meaningful means putting
    // uncovered: one on every file, which is NFR-LIC-060's (a Should's) work — accepted on that
    // uncovered: basis by the requirement's own Consequence note (FRS, 2026-08-12); closes M8
    #[test]
    fn both_licence_files_are_present_and_carry_their_own_text() {
        let root = real_root();
        for (label, required) in LICENCE_FILES {
            let violations = check_document(&root, label, &required);
            assert!(violations.is_empty(), "{violations:#?}");
        }
    }

    #[test]
    fn a_deleted_or_emptied_licence_file_is_a_violation() {
        // The negative control, in both shapes: absent, and present but not carrying the licence
        // it is named for. An existence-only check would pass the second.
        let dir = std::env::temp_dir().join(format!("xtask-lic-010-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let (label, required) = LICENCE_FILES[0];
        assert_eq!(check_document(&dir, label, &required).len(), 1);

        std::fs::write(dir.join(label), "").unwrap();
        assert_eq!(check_document(&dir, label, &required).len(), 2);

        // The Apache text under the MIT filename: present, non-empty, and still wrong.
        std::fs::write(
            dir.join(label),
            "Apache License\n\nCopyright (c) 2026 Erwan Patrick Legrand\n",
        )
        .unwrap();
        let violations = check_document(&dir, label, &required);
        assert_eq!(violations.len(), 1, "{violations:#?}");
        assert!(violations[0].contains("MIT License"), "{violations:#?}");

        std::fs::remove_dir_all(&dir).ok();
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
    fn generated_artifact_check_names_the_write_command_when_stale_or_absent() {
        let dir = std::env::temp_dir().join(format!("xtask-identity-blob-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("crates/namir-ui/src/brand")).unwrap();

        let absent = check_generated(&dir, BLOB_PATH, b"expected");
        assert_eq!(absent.len(), 1);
        assert!(absent[0].contains("cargo run -p xtask -- identity --write"));

        std::fs::write(dir.join(BLOB_PATH), b"something else").unwrap();
        let stale = check_generated(&dir, BLOB_PATH, b"expected");
        assert_eq!(stale.len(), 1);
        assert!(stale[0].contains("stale"));
        assert!(stale[0].contains("cargo run -p xtask -- identity --write"));

        std::fs::write(dir.join(BLOB_PATH), b"expected").unwrap();
        assert!(check_generated(&dir, BLOB_PATH, b"expected").is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    // --- M13: FR-UI-110's application icon -------------------------------------------------------

    #[test]
    fn the_icon_crop_is_the_rightmost_square() {
        // The shipped case: a 1767x474 wordmark yields the 474x474 square at its right-hand end.
        assert_eq!(icon_crop(1767, 474), (1293, 0, 474));
        // A square source is cropped to itself, with no gutter column to its left.
        assert_eq!(icon_crop(96, 96), (0, 0, 96));
        // A source taller than it is wide is cut from its vertical centre, and is still total.
        assert_eq!(icon_crop(100, 300), (0, 100, 100));
    }

    #[test]
    fn crop_takes_the_named_rectangle_and_does_not_wrap_rows() {
        let src: Vec<u8> = (0..12).collect(); // 4x3, row-major
        assert_eq!(crop_alpha(&src, 4, 2, 1, 2, 2), vec![6, 7, 10, 11]);
        // An over-wide request narrows rather than reading into the following row.
        assert_eq!(crop_alpha(&src, 4, 3, 0, 3, 2), vec![3, 7]);
        // Rows past the end of the buffer read as zero rather than panicking.
        assert_eq!(crop_alpha(&src, 4, 0, 2, 2, 2), vec![8, 9, 0, 0]);
    }

    /// The crop rule is a claim about the artwork's layout, and this is the check that makes a
    /// violation of it an error rather than an icon with a letter in the corner.
    #[test]
    fn a_wordmark_reaching_into_the_crop_is_refused_by_name() {
        // 200x100: the crop is the rightmost 100 columns, so the gutter is column 99. Fill it
        // completely and the premise is false.
        let mut alpha = vec![0u8; 200 * 100];
        for y in 0..100 {
            alpha[y * 200 + 99] = 0xff;
        }
        let err = check_icon_gutter(&alpha, 200, 100, 100).unwrap_err();
        assert!(err.contains("column 99"), "{err}");
        assert!(err.contains("reaches into that square"), "{err}");

        // The same column at 4 % coverage clears the 5 % limit.
        let mut sparse = vec![0u8; 200 * 100];
        for y in 0..4 {
            sparse[y * 200 + 99] = 0xff;
        }
        assert!(check_icon_gutter(&sparse, 200, 100, 100).is_ok());

        // A square source has no gutter column at all, which is not a violation.
        assert!(check_icon_gutter(&[], 100, 100, 0).is_ok());
    }

    /// The container, asserted against a synthetic two-pixel-wide source so the numbers can be
    /// computed by hand rather than read back from the encoder.
    #[test]
    fn the_ico_container_frames_every_size_it_declares() {
        let png = synthetic_rgba_png(8, 8, |_, _| [0xff, 0x66, 0x00, 0xff]);
        let ico = render_ico(&png).unwrap();

        assert_eq!(&ico[0..2], &0u16.to_le_bytes(), "reserved");
        assert_eq!(&ico[2..4], &1u16.to_le_bytes(), "type 1 = icon");
        assert_eq!(&ico[4..6], &(ICO_SIZES.len() as u16).to_le_bytes());

        let mut expected_offset = 6 + 16 * ICO_SIZES.len();
        for (i, &size) in ICO_SIZES.iter().enumerate() {
            let entry = &ico[6 + 16 * i..6 + 16 * (i + 1)];
            assert_eq!(entry[0], ico_entry_dimension(size), "width byte, {size}px");
            assert_eq!(entry[1], ico_entry_dimension(size), "height byte, {size}px");
            assert_eq!(&entry[4..6], &1u16.to_le_bytes(), "planes, {size}px");
            assert_eq!(&entry[6..8], &32u16.to_le_bytes(), "bit depth, {size}px");

            let len = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
            let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
            let side = size as usize;
            assert_eq!(
                len,
                40 + side * side * 4 + side.div_ceil(32) * 4 * side,
                "declared length, {size}px"
            );
            assert_eq!(offset, expected_offset, "declared offset, {size}px");
            expected_offset += len;

            // The header the entry points at must describe the size the entry declares, with the
            // doubled height the format requires of a colour-plus-mask image.
            let header = &ico[offset..offset + 40];
            assert_eq!(&header[0..4], &40u32.to_le_bytes(), "biSize, {size}px");
            assert_eq!(&header[4..8], &(size as i32).to_le_bytes(), "biWidth");
            assert_eq!(
                &header[8..12],
                &((size as i32) * 2).to_le_bytes(),
                "biHeight"
            );
            assert_eq!(&header[16..20], &0u32.to_le_bytes(), "BI_RGB, {size}px");
        }
        assert_eq!(
            expected_offset,
            ico.len(),
            "no slack between or after images"
        );
    }

    /// 256 is the largest edge an `.ico` can express, and it is spelled `0` — a one-byte field that
    /// silently truncates a larger size to something meaningless.
    #[test]
    fn the_largest_size_is_spelled_zero_in_the_directory() {
        assert_eq!(ico_entry_dimension(16), 16);
        assert_eq!(ico_entry_dimension(255), 255);
        assert_eq!(ico_entry_dimension(256), 0);
    }

    /// The colour bitmap is bottom-up straight-alpha BGRA. Driven at 16x16 against a source whose
    /// alpha is a plain vertical split, so a flipped or premultiplied encoding is visible in the
    /// bytes rather than only on a screen.
    #[test]
    fn the_colour_bitmap_is_bottom_up_straight_alpha_bgra() {
        // 2x2 mask: opaque top row, transparent bottom row.
        let mask = [0xffu8, 0xff, 0x00, 0x00];
        let image = ico_image(2, &mask);

        // Rows are emitted bottom-up, so the first row of pixel data is the mask's *last* row.
        let px = &image[40..40 + 16];
        assert_eq!(px[0..4], [0x00, 0x66, 0xff, 0x00], "B, G, R, A");
        assert_eq!(px[4..8], [0x00, 0x66, 0xff, 0x00]);
        assert_eq!(px[8..12], [0x00, 0x66, 0xff, 0xff]);
        assert_eq!(px[12..16], [0x00, 0x66, 0xff, 0xff]);

        // Straight, not premultiplied: a fully transparent pixel keeps the brand colour rather
        // than collapsing to black.
        assert_eq!(px[0..3], [MARK_FILL[2], MARK_FILL[1], MARK_FILL[0]]);

        // The AND mask follows, bottom-up and 4-byte aligned: 1 where the pixel is transparent.
        let and = &image[40 + 16..];
        assert_eq!(and.len(), 8, "two rows of four bytes");
        assert_eq!(and[0], 0b1100_0000, "the transparent row, first");
        assert_eq!(and[4], 0b0000_0000, "the opaque row");
    }

    /// Why the pipeline is a plain area average and nothing else, recorded as a measurement rather
    /// than as a comment because a contrast-rescaling step was written, measured here, and deleted.
    ///
    /// The mark is line art reduced 29.6x to reach 16x16, so most of that tile is a thin stroke
    /// averaged against its background and the icon reads pale. The obvious repair — rescale each
    /// size so its strongest pixel is opaque — turns out to buy **nothing**: the peak is already
    /// 243 of 255, because the head's solid regions survive the reduction even where its strokes do
    /// not. The gain would be 1.05x, invisible. This test pins that number so the idea is not
    /// re-invented, and states what would actually work: a simplified, icon-specific piece of
    /// artwork, which is an artwork decision and not a downsampler one.
    #[test]
    fn a_contrast_rescale_would_not_help_the_smallest_size_and_is_not_applied() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ has a parent");
        let png = std::fs::read(root.join(MARK_SOURCE_PATH)).expect("the artwork is checked in");
        let (w, h, alpha) = decode_alpha(&png).unwrap();
        let (x0, y0, side) = icon_crop(w, h);
        let mark = crop_alpha(&alpha, w, x0, y0, side, side);

        let smallest = downsample_alpha(&mark, side, side, ICO_SIZES[0], ICO_SIZES[0]);
        assert_eq!(*smallest.iter().max().unwrap(), 243);
        // The larger sizes are opaque somewhere, so a rescale is exactly the identity for them.
        let largest = downsample_alpha(&mark, side, side, 256, 256);
        assert_eq!(*largest.iter().max().unwrap(), 255);
    }

    #[test]
    fn render_ico_is_byte_identical_across_repeated_renders() {
        // Shaped like the real artwork -- content only in the rightmost square, so the gutter is
        // clear -- rather than uniformly noisy, which the crop rule's own guard would refuse.
        let png = synthetic_rgba_png(64, 32, |x, y| {
            let alpha = if x < 32 {
                0
            } else {
                ((x * 7 + y * 11) % 256) as u8
            };
            [0xff, 0x66, 0x00, alpha]
        });
        assert_eq!(render_ico(&png).unwrap(), render_ico(&png).unwrap());
    }

    /// The icon inherits every input check the blob has, because it decodes through the same
    /// function; asserted rather than assumed, since the two now have separate entry points.
    #[test]
    fn render_ico_refuses_the_same_artwork_render_blob_does() {
        let grayscale = synthetic_grayscale_png(8, 8);
        assert!(render_ico(&grayscale).unwrap_err().contains("8-bit RGBA"));

        let multi = synthetic_rgba_png(8, 8, |x, _| {
            if x < 4 {
                [0xff, 0x66, 0x00, 0xff]
            } else {
                [0x00, 0x66, 0xff, 0xff]
            }
        });
        assert!(render_ico(&multi).unwrap_err().contains("colour-lossless"));
    }

    /// The counterpart of [`the_shipped_artwork_is_a_single_fill`]: the crop rule's premise is
    /// asserted against the real artwork, not only against synthetic inputs, because the rule is
    /// only correct for artwork laid out the way this one is.
    #[test]
    fn the_shipped_artwork_puts_a_square_mark_at_its_right_hand_end() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ has a parent");
        let png = std::fs::read(root.join(MARK_SOURCE_PATH)).expect("the artwork is checked in");
        let (w, h, alpha) = decode_alpha(&png).expect("the shipped artwork is a single fill");

        let (x0, y0, side) = icon_crop(w, h);
        assert_eq!((x0, y0, side), (1293, 0, 474));
        check_icon_gutter(&alpha, w, h, x0).expect("the wordmark must not reach into the crop");

        // The measurement ICON_GUTTER_MAX_COVERAGE_PERCENT's doc comment is written from, pinned so
        // that artwork drifting toward the limit is visible before it crosses it.
        assert_eq!(column_ink(&alpha, w, h, x0 - 1), 9);
        // ... and the wordmark's `r`, well clear of the crop and well above the limit.
        assert_eq!(column_ink(&alpha, w, h, 1275), 77);
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
