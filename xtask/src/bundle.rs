//! `cargo run -p xtask -- bundle [--target <windows|macos|linux>] [--check | --plan]`: M13's first
//! deliverable and the primitive D-18.3's release pipeline is built on — **build → `xtask bundle`
//! → per-OS package → GitHub Release**, in that order, with everything after the arrow depending
//! on this step.
//!
//! It assembles a per-platform *staging tree* from artifacts a release build has already produced:
//! the CLAP artifact in the form that platform's loader requires, the standalone (a plain
//! executable on Windows and Linux, an application bundle on macOS — see below), and the three
//! documents FR-PKG-040 requires every distribution to carry (plus `README.md`). It
//! builds nothing, archives nothing and hashes nothing — the per-OS packagers consume this tree,
//! and the archive/hash step is `release.yml`'s (D-18.3), not this subcommand's.
//!
//! # Why this exists at all
//!
//! Nothing in the Rust ecosystem will build a macOS `.clap`. `cargo` produces a `.dylib`, and on
//! macOS a `.clap` is a **bundle directory** —
//! `Namir.clap/Contents/{Info.plist, PkgInfo, MacOS/<dylib>}` — not a renamed shared library.
//! That is CLAP's own `entry.h` definition of `plugin_path` (the DSO on Windows and Linux, the
//! bundle on macOS), not a macOS convention layered over it, so a renamed `libnamir_clap.dylib` is
//! something no host will load. D-13.3's *Consequence (added M8-planning)* and **FR-PKG-020** both
//! carry that rule; `docs/user-guide.md` is its written form, and this module is the executable
//! one. `nih_plug_xtask` is the model followed: a bundler subcommand driven by a description of
//! what to produce, living in tooling that already exists, so the same command runs locally and in
//! CI and a developer can reproduce a release artifact without reading the workflow file.
//!
//! # The planner is pure, and that is load-bearing
//!
//! [`plan`] takes a [`Platform`] and returns a [`Layout`]. It reads nothing, touches no
//! filesystem, and never consults the host it is running on — so the macOS layout can be computed,
//! asserted and (given macOS artifacts) materialised from a Windows or Linux machine. This is what
//! lets FR-PKG-020's `Verify: S` assertion be an ordinary test that runs on **every** CI runner
//! rather than only on the macOS leg of a release job, which would leave the one platform whose
//! layout is easy to get wrong checked only where the check is hardest to reach.
//!
//! # No new dependency, so `Info.plist` is emitted as text
//!
//! §17's note on build tooling draws the line this module stays on the right side of: a non-cargo
//! build tool needs no register row, a **cargo dependency** does (the `png` row `identity.rs` took
//! is the precedent). A plist is XML with a fixed shape and a handful of keys here, so [`plist`]
//! writes it directly and deterministically, in sorted-key order, rather than taking a plist crate
//! for it. `PkgInfo` is eight literal bytes.
//!
//! # Two macOS bundles, not one
//!
//! macOS is the only platform where **both** shipped artifacts are bundles. `Namir.clap` is one
//! because CLAP says so; `Namir.app` is one because an unbundled process cannot declare
//! `NSMicrophoneUsageDescription`, without which macOS 10.14+ denies it the audio input device —
//! and `namir-app` opens one on its ordinary path, this being an instrument-input amp simulator.
//! A bare `namir` binary staged on macOS is therefore a *broken product*, not an untidy one; it is
//! also what D-18.3's "the standalone app under `/Applications`" cannot mean, since a bare Mach-O
//! double-clicked from there merely opens Terminal. Windows and Linux are unchanged: the
//! standalone is a plain executable on both.
//!
//! # Argument parsing is strict
//!
//! Like `traceability` and `nam-parity`, and unlike the lenient `any(|a| a == "--write")` the
//! generate-and-diff subcommands use (`main.rs`'s own note on the house rule): the flags here
//! select between *behaviours* — materialise, assert, or describe — and between *target
//! platforms*, so a typo that silently selected a different one of those would be worse than a
//! loud refusal. Anything unrecognised exits 2.

use std::path::{Path, PathBuf};

/// The **plugin** bundle's `CFBundleIdentifier`.
///
/// Deliberately the **same string** `namir-clap` already declares as its CLAP plugin id
/// (`crates/namir-clap/src/lib.rs`'s `PLUGIN_ID`). One artifact, one reverse-DNS identity;
/// inventing a second here would mean a host and the operating system disagreeing about what this
/// artifact is. It is a second copy of the literal rather than an import because `xtask` is in
/// neither shipped product's dependency graph — the same reason `identity.rs` re-states
/// `MARK_FILL`.
pub const PLUGIN_BUNDLE_IDENTIFIER: &str = "org.legrand.namir";

/// The **application** bundle's `CFBundleIdentifier`, and *not* the same string as
/// [`PLUGIN_BUNDLE_IDENTIFIER`].
///
/// **Decision, M13, recorded here because this is where the constant lives.** A bundle identifier
/// must be unique per bundle — two bundles sharing one is a documented way to confuse
/// LaunchServices about which of them a given identifier resolves to — and this identifier is not
/// cosmetic on macOS: **TCC keys the microphone grant on it**. If the standalone and the plugin
/// shared an identifier, the permission the user grants the standalone would be recorded against
/// the same subject as the plugin bundle, and a revocation or a re-signing of either would move
/// the other's grant with it. `.standalone` as the suffix, rather than `.app` (which reads as the
/// bundle extension, not as a product) or a sibling like `org.legrand.namir-standalone` (which
/// breaks the prefix relationship a reverse-DNS tree is for): it names what the artifact **is**,
/// and it keeps the plugin's identifier as the product's root so a future third artifact extends
/// the same tree.
pub const APP_BUNDLE_IDENTIFIER: &str = "org.legrand.namir.standalone";

/// `CFBundleName`, and the stem of the artifact every platform's loader looks for.
pub const BUNDLE_NAME: &str = "Namir";

/// `CFBundleShortVersionString` and `CFBundleVersion` for the application bundle.
///
/// A literal rather than a `cargo metadata` lookup, so that [`plan`] stays pure — and kept honest
/// by `the_bundled_version_tracks_namir_apps_own`, which reads `crates/namir-app/Cargo.toml` and
/// fails if the two ever disagree. `identity.rs`'s `MARK_FILL` plus
/// `the_shipped_artwork_is_a_single_fill` is the same device: duplicate the constant, then assert
/// the duplication against the real artifact rather than trusting it.
///
/// The two keys carry the same value deliberately. `CFBundleShortVersionString` is the
/// user-visible release version and `CFBundleVersion` the build number; with no build-number
/// scheme in this project (and none needed before signed, notarised releases exist), inventing a
/// second sequence here would be a scheme nothing increments.
pub const PRODUCT_VERSION: &str = "0.1.0";

/// `LSMinimumSystemVersion` for the application bundle.
///
/// **Decision, M13, recorded here because this is where the constant lives.** This is *derived*,
/// not guessed, but from the toolchain rather than from any statement in this repository — nothing
/// in the FRS, the architecture document or any manifest states a macOS floor, and neither
/// `baseview` 0.2.2, `egui-baseview` 0.6.0 nor the pinned `cpal` fork states one either (checked
/// this pass: no `MACOSX_DEPLOYMENT_TARGET`, no documented minimum, and `baseview`'s macOS backend
/// uses nothing newer than `NSOpenGLView`, which is ancient and merely deprecated).
///
/// The floor that *is* real comes from the target triple CI actually builds: `.github/workflows/
/// ci.yml`'s macOS leg runs on `macos-latest`, which is Apple Silicon, so the artifact is
/// `aarch64-apple-darwin` — a Rust tier-1 target whose own minimum is **macOS 11.0**, because
/// Apple Silicon Macs shipped with Big Sur and no earlier macOS runs on them at all. An
/// `x86_64-apple-darwin` build would have a lower floor (Rust's baseline there is 10.12), so if a
/// universal or Intel-only artifact is ever published this constant is the thing to revisit — but
/// declaring 11.0 for an arm64-only build understates nothing.
///
/// Two lower bounds sit under it and are both satisfied: `NSMicrophoneUsageDescription` is
/// enforced from macOS 10.14, and the hardened-runtime/notarisation path R-11 defers is 10.14+
/// too. So 11.0 is above every constraint this product actually has.
pub const MINIMUM_MACOS_VERSION: &str = "11.0";

/// `NSMicrophoneUsageDescription`: the sentence macOS shows the user in the permission prompt, in
/// their own words rather than in the developer's.
///
/// **Not optional, and the reason this bundle exists at all.** From macOS 10.14 a process may not
/// open an audio *input* device until the user has granted microphone access, and a process that
/// declares no usage description is denied outright rather than prompted. `namir-app` opens an
/// input device on its ordinary path (`crates/namir-app/src/audio_io.rs`'s `input_devices` /
/// `Direction::Input`) — it is an instrument-input amp simulator, so that is the whole product —
/// and an **unbundled** Mach-O has no `Info.plist` to declare this in. A bare `namir` binary
/// therefore does not merely look unpolished on macOS: it cannot capture the instrument signal.
///
/// The plugin bundle deliberately does *not* declare this. A plugin does not open the device; the
/// host process does, under the host's own grant, and a usage description in a loaded bundle's
/// `Info.plist` is not what TCC consults.
pub const MICROPHONE_USAGE_DESCRIPTION: &str = "Namir processes the live signal from your guitar or bass, so it needs access to the audio \
     input device you select.";

/// The CLAP artifact's name in every staging tree: a file on Windows and Linux, a bundle directory
/// on macOS (see this module's header).
pub const CLAP_ARTIFACT: &str = "Namir.clap";

/// The standalone application's name in a **macOS** staging tree, where it is an application
/// bundle. On Windows and Linux the standalone stays a plain executable
/// ([`Platform::standalone`]) and this constant is unused.
pub const APP_ARTIFACT: &str = "Namir.app";

/// `PkgInfo` is an eight-byte pair: the four-byte package type, then the four-byte creator code.
/// It carries the same two values as `CFBundlePackageType` and `CFBundleSignature`, so the two must
/// agree — a bundle whose `PkgInfo` and `Info.plist` disagree about its type is malformed. The
/// creator code is `????`, the documented "unset" value: creator codes were retired with the
/// Carbon-era registry and Apple's own templates have emitted `????` for years.
///
/// No trailing newline in either — each file is exactly eight bytes.
///
/// A **plugin** bundle is `BNDL` (a loadable bundle, `CFBundlePackageType` `BNDL`); an
/// **application** bundle is `APPL`. This is the one place the two macOS artifacts this subcommand
/// stages genuinely differ in kind, so they are separate constants rather than one with a
/// substitution.
pub const PLUGIN_PKG_INFO: &str = "BNDL????";

/// See [`PLUGIN_PKG_INFO`]: an application is `APPL`, not `BNDL`.
pub const APP_PKG_INFO: &str = "APPL????";

/// FR-PKG-040's enumerated set, exactly: NFR-LIC-030's machine-generated attribution file, and the
/// full text of both licences of NFR-LIC-010. Named as its own constant so the requirement's set is
/// legible as itself rather than as a prefix of the longer list [`staged_documents`] returns.
pub const LICENCE_DOCUMENTS: [&str; 3] =
    ["THIRD-PARTY-NOTICES.md", "LICENSE-MIT", "LICENSE-APACHE"];

/// Staged beside [`LICENCE_DOCUMENTS`] though FR-PKG-040 does not ask for it: a distribution with
/// no statement of what it is is a worse artifact than one with a redundant file in it.
pub const README: &str = "README.md";

/// Also staged beside [`LICENCE_DOCUMENTS`], and for a sharper reason than [`README`]'s.
///
/// The shipped binaries **carry the brand mark** — M12 embedded it, and `namir-ui` `include_bytes!`s
/// the generated alpha blob — and **NFR-LIC-070** requires the terms on which the name and mark may
/// be used to be stated explicitly, precisely because a permissive code licence beside an unstated
/// trademark position is the combination that produces awkward conversations later. `README.md`'s
/// licence section points at `TRADEMARK.md`; staging the README without it makes that pointer
/// dangle in every distribution, which is the one way a distribution can be *worse* than no
/// document at all: it names a file and then does not carry it.
///
/// It is deliberately **not** in [`LICENCE_DOCUMENTS`]. FR-PKG-040's set is exactly three, named by
/// the requirement, and widening a requirement's own enumeration to hold a file it does not mention
/// is how a set stops meaning anything.
pub const TRADEMARK: &str = "TRADEMARK.md";

/// Every document a staging tree carries, in staging order.
pub fn staged_documents() -> [&'static str; 5] {
    let [notices, mit, apache] = LICENCE_DOCUMENTS;
    [notices, mit, apache, README, TRADEMARK]
}

/// The one remedy line every violation ends with, in one place so the check and its tests cannot
/// drift apart on the exact command a reader is told to run (`identity.rs`'s `BLOB_REMEDY` is the
/// same device for the same reason).
pub const BUNDLE_REMEDY: &str =
    "Run `cargo build --release --workspace` then `cargo run -p xtask -- bundle` to produce it.";

/// A platform a staging tree can be produced for. Not the host — see [`plan`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

impl Platform {
    /// Every platform, in the order the usage line and the tests enumerate them.
    pub const ALL: [Platform; 3] = [Platform::Windows, Platform::MacOs, Platform::Linux];

    /// The platform this process is running on, from `std::env::consts::OS` rather than a
    /// `#[cfg(target_os)]`. Two reasons: `xtask` is dev tooling that has no business carrying
    /// platform `cfg` even though D-5.2's lint does not reach it, and a runtime lookup keeps the
    /// *whole* module compiled — and therefore type-checked and tested — on every host, which is
    /// the same property [`plan`]'s purity buys at the layout level.
    pub fn host() -> Result<Platform, String> {
        Platform::parse(std::env::consts::OS).map_err(|_| {
            format!(
                "bundle: this host's OS ({}) is not one Namir is packaged for -- pass --target \
                 <windows|macos|linux> to name one explicitly",
                std::env::consts::OS
            )
        })
    }

    /// Parses `--target`'s value. Also parses `std::env::consts::OS`, which is exactly these three
    /// spellings on the three platforms concerned.
    pub fn parse(name: &str) -> Result<Platform, String> {
        match name {
            "windows" => Ok(Platform::Windows),
            "macos" => Ok(Platform::MacOs),
            "linux" => Ok(Platform::Linux),
            other => Err(format!(
                "bundle: unknown target `{other}` (expected one of {})",
                Platform::ALL.map(Platform::name).join(", ")
            )),
        }
    }

    /// The spelling [`Platform::parse`] accepts, and the staging tree's directory name.
    pub fn name(self) -> &'static str {
        match self {
            Platform::Windows => "windows",
            Platform::MacOs => "macos",
            Platform::Linux => "linux",
        }
    }

    /// The file name `cargo build --release` gives `namir-clap`'s `cdylib` on this platform.
    pub fn clap_library(self) -> &'static str {
        match self {
            Platform::Windows => "namir_clap.dll",
            Platform::MacOs => "libnamir_clap.dylib",
            Platform::Linux => "libnamir_clap.so",
        }
    }

    /// The file name `cargo build --release` gives `namir-app`'s `[[bin]] name = "namir"`.
    pub fn standalone(self) -> &'static str {
        match self {
            Platform::Windows => "namir.exe",
            Platform::MacOs | Platform::Linux => "namir",
        }
    }

    /// Whether the CLAP artifact is a bundle directory (macOS) or the shared library itself
    /// (Windows, Linux) — the one structural difference between the three layouts.
    pub fn clap_is_a_bundle(self) -> bool {
        matches!(self, Platform::MacOs)
    }
}

/// Where one staged file's bytes come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Copied from the release build directory, by the file name `cargo` produced.
    Build(String),
    /// Copied from the repository root, by name.
    Repo(String),
    /// Written by this subcommand, byte for byte — and therefore also byte-comparable by
    /// [`check`], which is what makes a hand-edited `Info.plist` a violation rather than a silent
    /// difference between what was shipped and what was described.
    Generated(String),
}

/// One file a staging tree must contain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Slash-separated, relative to the staging root. Always a file; the only directories in a
    /// layout are the ones a destination path implies.
    pub dest: String,
    pub source: Source,
}

/// Everything a staging tree for one platform must contain. Produced by [`plan`], consumed by
/// [`materialise`] and [`check`] — the same value drives both, which is why "what was produced"
/// and "what is asserted" cannot drift apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub platform: Platform,
    pub entries: Vec<Entry>,
}

/// The staging tree for `platform`, computed **without reference to the host**.
///
/// Pure: no I/O, no `std::env`, no `cfg!`. Calling `plan(Platform::MacOs)` on Windows returns the
/// real macOS layout, which is the property FR-PKG-020's test rests on (see this module's header).
pub fn plan(platform: Platform) -> Layout {
    let library = platform.clap_library();
    let mut entries = Vec::new();

    let standalone = platform.standalone();

    if platform.clap_is_a_bundle() {
        entries.push(Entry {
            dest: format!("{CLAP_ARTIFACT}/Contents/Info.plist"),
            source: Source::Generated(plugin_info_plist(library)),
        });
        entries.push(Entry {
            dest: format!("{CLAP_ARTIFACT}/Contents/PkgInfo"),
            source: Source::Generated(PLUGIN_PKG_INFO.to_string()),
        });
        entries.push(Entry {
            dest: format!("{CLAP_ARTIFACT}/Contents/MacOS/{library}"),
            source: Source::Build(library.to_string()),
        });
        entries.push(Entry {
            dest: format!("{APP_ARTIFACT}/Contents/Info.plist"),
            source: Source::Generated(app_info_plist(standalone)),
        });
        entries.push(Entry {
            dest: format!("{APP_ARTIFACT}/Contents/PkgInfo"),
            source: Source::Generated(APP_PKG_INFO.to_string()),
        });
        entries.push(Entry {
            dest: format!("{APP_ARTIFACT}/Contents/MacOS/{standalone}"),
            source: Source::Build(standalone.to_string()),
        });
    } else {
        entries.push(Entry {
            dest: CLAP_ARTIFACT.to_string(),
            source: Source::Build(library.to_string()),
        });
        entries.push(Entry {
            dest: standalone.to_string(),
            source: Source::Build(standalone.to_string()),
        });
    }
    for document in staged_documents() {
        entries.push(Entry {
            dest: document.to_string(),
            source: Source::Repo(document.to_string()),
        });
    }

    Layout { platform, entries }
}

/// The **plugin** bundle's `Info.plist`.
///
/// Four keys, which is what `docs/user-guide.md` specifies as the minimum and what a loader
/// actually reads: `CFBundleExecutable` (the dylib inside `Contents/MacOS`), `CFBundleIdentifier`,
/// `CFBundleName` and `CFBundlePackageType` (`BNDL`).
///
/// Deliberately **no** `NSMicrophoneUsageDescription` — see [`MICROPHONE_USAGE_DESCRIPTION`] for
/// why a plugin neither needs nor could use one.
pub fn plugin_info_plist(executable: &str) -> String {
    plist(&[
        ("CFBundleExecutable", PlistValue::Text(executable)),
        (
            "CFBundleIdentifier",
            PlistValue::Text(PLUGIN_BUNDLE_IDENTIFIER),
        ),
        ("CFBundleName", PlistValue::Text(BUNDLE_NAME)),
        ("CFBundlePackageType", PlistValue::Text("BNDL")),
    ])
}

/// The **application** bundle's `Info.plist`.
///
/// Ten keys. Four are the same shape as the plugin's, with `CFBundlePackageType` **`APPL`** and its
/// own [`APP_BUNDLE_IDENTIFIER`]; the rest are what an application needs and a loadable bundle does
/// not: the two version keys, [`MINIMUM_MACOS_VERSION`], `CFBundleInfoDictionaryVersion` (`6.0`,
/// what every Apple template carries), `NSHighResolutionCapable` (an egui window rendered at 1x and
/// upscaled on a Retina display is visibly soft, and this key is the only way an artifact assembled
/// outside Xcode declares otherwise), and [`MICROPHONE_USAGE_DESCRIPTION`] — the last of which is
/// the reason this bundle exists at all.
///
/// The key set is a deliberate transcription of the stopgap `packaging/macos/make_installer.sh`
/// wrote while `xtask bundle` staged a bare executable, whose own comment asked for exactly that
/// ("a transcription rather than a redesign") so that its `if [ -d …Namir.app ]` branch takes its
/// other arm with no change to that script. The one value that differs is the usage description,
/// which is the sentence a user reads in a permission dialogue and is worth more than a stopgap's
/// placeholder.
///
/// Not declared, and worth naming so its absence is a recorded choice rather than an oversight:
/// **`CFBundleIconFile`**. There is no `.icns` in this repository — `images/namir.png` is the brand
/// mark M12 shipped, and D-17.3 books the *Windows* `.exe` icon to M13's packaging pipeline. A
/// plist key naming an icon file that is not in the bundle is worse than no key, so the icon lands
/// with the icon deliverable, and this function gains an eleventh key then.
pub fn app_info_plist(executable: &str) -> String {
    plist(&[
        ("CFBundleExecutable", PlistValue::Text(executable)),
        (
            "CFBundleIdentifier",
            PlistValue::Text(APP_BUNDLE_IDENTIFIER),
        ),
        ("CFBundleInfoDictionaryVersion", PlistValue::Text("6.0")),
        ("CFBundleName", PlistValue::Text(BUNDLE_NAME)),
        ("CFBundlePackageType", PlistValue::Text("APPL")),
        (
            "CFBundleShortVersionString",
            PlistValue::Text(PRODUCT_VERSION),
        ),
        ("CFBundleVersion", PlistValue::Text(PRODUCT_VERSION)),
        (
            "LSMinimumSystemVersion",
            PlistValue::Text(MINIMUM_MACOS_VERSION),
        ),
        ("NSHighResolutionCapable", PlistValue::Boolean(true)),
        (
            "NSMicrophoneUsageDescription",
            PlistValue::Text(MICROPHONE_USAGE_DESCRIPTION),
        ),
    ])
}

/// A plist value. Two variants because two are needed: `NSHighResolutionCapable` is a **boolean**,
/// and `<string>true</string>` is not the same document as `<true/>` — a reader that type-checks
/// (and `plutil -lint` does) is entitled to reject the former.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlistValue<'a> {
    Text(&'a str),
    Boolean(bool),
}

/// `entries` as a plist document, keys emitted in **sorted** order with tab indentation (`plutil`'s
/// own output convention), so that two runs on two machines produce identical bytes and [`check`]
/// can byte-compare rather than parse.
///
/// Sorted here rather than trusted from the caller: determinism is the property the byte comparison
/// rests on, and a caller that listed two keys out of order would otherwise make a checked-in
/// expectation depend on the order someone happened to type.
///
/// The `DOCTYPE`'s URL is a public identifier in a fixed string, resolved by nothing at build or
/// run time; it is what every `Info.plist` carries and is no network access of any kind.
fn plist(entries: &[(&str, PlistValue)]) -> String {
    let mut sorted: Vec<(&str, PlistValue)> = entries.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));

    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n",
    );
    for (key, value) in sorted {
        let rendered = match value {
            PlistValue::Text(text) => format!("<string>{}</string>", escape_xml(text)),
            PlistValue::Boolean(true) => "<true/>".to_string(),
            PlistValue::Boolean(false) => "<false/>".to_string(),
        };
        out.push_str(&format!("\t<key>{}</key>\n\t{rendered}\n", escape_xml(key)));
    }
    out.push_str("</dict>\n</plist>\n");
    out
}

/// The three characters that cannot appear literally in XML character data. None of the values
/// here contains one today; escaping anyway costs nothing and means a future usage description
/// written with an `&` produces a valid plist rather than one `plutil` rejects at package time.
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `<target>/release`, honouring `CARGO_TARGET_DIR` so a developer with a shared target directory
/// bundles what they just built rather than what is not there.
pub fn build_dir(repo_root: &Path) -> PathBuf {
    target_dir(repo_root).join("release")
}

/// `<target>/bundle/<platform>`. Per-platform, so producing a macOS tree on Windows (which
/// [`plan`] permits) cannot overwrite the host's own.
pub fn staging_root(repo_root: &Path, platform: Platform) -> PathBuf {
    target_dir(repo_root).join("bundle").join(platform.name())
}

fn target_dir(repo_root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| repo_root.join("target"), PathBuf::from)
}

/// An [`Entry::dest`] resolved against a staging root. Split on `/` and joined component by
/// component rather than joined whole, so a message about a bundle's innards reads in the host's
/// own separator instead of the mixed `...\macos\Namir.clap/Contents/...` a single join produces on
/// Windows.
fn dest_path(staging_root: &Path, dest: &str) -> PathBuf {
    dest.split('/')
        .fold(staging_root.to_path_buf(), |path, part| path.join(part))
}

/// Materialises `layout` under `staging_root`, from artifacts in `build_dir` and documents at
/// `repo_root`. Returns a one-line summary for CI logs, the contract `identity::write_blob` uses.
///
/// The tree is **removed and rebuilt** rather than updated in place: a stale artifact left over
/// from an earlier build is exactly the kind of thing a presence check would pass and a user would
/// then run, and a bundle directory that changed shape between releases would otherwise keep its
/// old files alongside its new ones.
///
/// `Err` names the first source it could not read, with [`BUNDLE_REMEDY`]'s prerequisite — a
/// missing `target/release/libnamir_clap.dylib` on a machine that has not built for macOS is the
/// ordinary case, not a defect in the layout.
pub fn materialise(
    repo_root: &Path,
    build_dir: &Path,
    staging_root: &Path,
    layout: &Layout,
) -> Result<String, String> {
    if staging_root.exists() {
        std::fs::remove_dir_all(staging_root)
            .map_err(|e| format!("failed to clear {}: {e}", staging_root.display()))?;
    }

    let mut copied = 0usize;
    let mut generated = 0usize;
    for entry in &layout.entries {
        let dest = dest_path(staging_root, &entry.dest);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
        }
        match &entry.source {
            Source::Build(name) => {
                copy_into(&build_dir.join(name), &dest)?;
                copied += 1;
            }
            Source::Repo(name) => {
                copy_into(&repo_root.join(name), &dest)?;
                copied += 1;
            }
            Source::Generated(contents) => {
                std::fs::write(&dest, contents)
                    .map_err(|e| format!("failed to write {}: {e}", dest.display()))?;
                generated += 1;
            }
        }
    }

    Ok(format!(
        "staged the {} layout at {} ({copied} file(s) copied, {generated} generated)",
        layout.platform.name(),
        staging_root.display()
    ))
}

fn copy_into(source: &Path, dest: &Path) -> Result<(), String> {
    std::fs::copy(source, dest).map(|_| ()).map_err(|e| {
        format!(
            "failed to copy {} to {}: {e}",
            source.display(),
            dest.display()
        )
    })
}

/// Every way the tree at `staging_root` deviates from `layout`, empty meaning it is the form the
/// platform's loader requires.
///
/// A **list** rather than a first failure, for the reason `identity::check` gives: a missing
/// `Info.plist` must not hide an absent licence text, since a reader who fixes one and re-runs has
/// learnt nothing about the other.
///
/// `Err` is reserved for an input that cannot be evaluated at all — no staging tree — as distinct
/// from a violation, which is a finding *about* a tree that exists.
pub fn check(staging_root: &Path, layout: &Layout) -> Result<Vec<String>, String> {
    if !staging_root.is_dir() {
        return Err(format!(
            "no staging tree at {}. {BUNDLE_REMEDY}",
            staging_root.display()
        ));
    }

    let mut violations = check_clap_form(staging_root, layout.platform);
    violations.extend(check_app_form(staging_root, layout.platform));
    for entry in &layout.entries {
        violations.extend(check_entry(staging_root, entry));
    }
    Ok(violations)
}

/// FR-PKG-020's structural half: the CLAP artifact is a **directory** on macOS and a **file** on
/// Windows and Linux. Checked on its own, before the per-entry checks, because a plain file named
/// `Namir.clap` on macOS is not a bundle missing a few pieces — it is the specific mistake the
/// requirement exists to catch, and it deserves to be reported as itself rather than as three
/// absent paths.
fn check_clap_form(staging_root: &Path, platform: Platform) -> Vec<String> {
    let path = staging_root.join(CLAP_ARTIFACT);
    let (is_dir, is_file) = (path.is_dir(), path.is_file());

    if platform.clap_is_a_bundle() {
        if is_dir {
            return Vec::new();
        }
        let found = if is_file {
            "a plain file -- a renamed dylib is something no host will load"
        } else {
            "nothing"
        };
        return vec![format!(
            "{}: on macOS the CLAP artifact must be a bundle directory (CLAP's own entry.h defines \
             plugin_path as the bundle there, not the shared library), found {found}. \
             {BUNDLE_REMEDY}",
            path.display()
        )];
    }

    if is_file {
        return Vec::new();
    }
    let found = if is_dir { "a directory" } else { "nothing" };
    vec![format!(
        "{}: on {} the CLAP artifact must be the shared library renamed to `{CLAP_ARTIFACT}`, \
         found {found}. {BUNDLE_REMEDY}",
        path.display(),
        platform.name()
    )]
}

/// The same structural check for the **application** bundle on macOS, and the mirror of it
/// elsewhere: `Namir.app` is a directory on macOS and must not exist at all on Windows or Linux,
/// where the standalone is a plain executable.
///
/// This one is **not** FR-PKG-020's: that requirement's text is about the CLAP artifact's form and
/// nothing else. It is D-18.3's — a release "places the standalone app under `/Applications`",
/// which a bare Mach-O does not satisfy — and, more concretely, it is what makes the standalone
/// work at all on macOS: see [`MICROPHONE_USAGE_DESCRIPTION`].
fn check_app_form(staging_root: &Path, platform: Platform) -> Vec<String> {
    let path = staging_root.join(APP_ARTIFACT);
    let (is_dir, is_file) = (path.is_dir(), path.is_file());

    if !platform.clap_is_a_bundle() {
        return if is_dir || is_file {
            vec![format!(
                "{}: an application bundle has no meaning on {} -- the standalone is the plain \
                 executable `{}` there. {BUNDLE_REMEDY}",
                path.display(),
                platform.name(),
                platform.standalone()
            )]
        } else {
            Vec::new()
        };
    }

    if is_dir {
        return Vec::new();
    }
    let found = if is_file {
        "a plain file -- an unbundled binary cannot declare NSMicrophoneUsageDescription, so macOS \
         denies it the audio input device, and double-clicking it opens Terminal"
    } else {
        "nothing"
    };
    vec![format!(
        "{}: on macOS the standalone must be an application bundle, found {found}. \
         {BUNDLE_REMEDY}",
        path.display()
    )]
}

fn check_entry(staging_root: &Path, entry: &Entry) -> Vec<String> {
    let path = dest_path(staging_root, &entry.dest);
    if !path.is_file() {
        return vec![format!(
            "{}: required by the layout and {}. {BUNDLE_REMEDY}",
            path.display(),
            if path.is_dir() {
                "present as a directory rather than a file"
            } else {
                "absent from the staging tree"
            }
        )];
    }

    let Source::Generated(expected) = &entry.source else {
        return Vec::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(actual) if &actual == expected => Vec::new(),
        Ok(_) => vec![format!(
            "{}: does not match the text this subcommand generates -- it is generated, not \
             hand-edited. {BUNDLE_REMEDY}",
            path.display()
        )],
        Err(e) => vec![format!("{}: could not be read ({e})", path.display())],
    }
}

/// What one `bundle` invocation was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Produce the staging tree, then assert it (the default).
    Materialise,
    /// Assert an existing staging tree, touching nothing.
    Check,
    /// Print the layout and exit. Reads and writes nothing, so it works for any `--target` on any
    /// host — the console form of [`plan`]'s purity.
    Plan,
    /// Assert a tree that came **out of a produced distribution** — an unpacked ZIP, an unpacked
    /// tarball, an expanded `.pkg` or a mounted `.dmg` — rather than the staging tree that went
    /// into one.
    ///
    /// Mechanically this is [`Mode::Check`] against a caller-named directory instead of the one
    /// [`staging_root`] derives, and it is a separate variant because the *claim* differs and a
    /// reader of a CI log must be able to tell which was made. `--check` says "what we are about
    /// to hand the packager is the right shape"; `--inspect` says "what came back out of it still
    /// is", and only the second can catch a packaging step that drops a file. M14 Phase 5 added
    /// it so that FR-PKG-040's "every distribution … shall contain" is asserted against something
    /// a user could actually download.
    Inspect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleArgs {
    pub platform: Platform,
    pub mode: Mode,
    /// The directory [`Mode::Inspect`] was pointed at. `None` in every other mode, where the tree
    /// is [`staging_root`]'s and is derived rather than named.
    pub tree: Option<PathBuf>,
}

/// Parses `bundle`'s own argument list (everything after the `bundle` token), strictly: see this
/// module's header for why an unrecognised flag here is refused rather than ignored.
pub fn parse_args(args: &[String]) -> Result<BundleArgs, String> {
    let mut platform = None;
    let mut mode = None;
    let mut tree = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--inspect" => {
                let Some(value) = iter.next() else {
                    return Err(
                        "bundle: `--inspect` needs the directory a produced distribution was \
                         unpacked into"
                            .to_string(),
                    );
                };
                if tree.is_some() {
                    return Err("bundle: `--inspect` given more than once".to_string());
                }
                if mode.is_some_and(|existing| existing != Mode::Inspect) {
                    return Err(
                        "bundle: `--inspect` selects a different behaviour from `--check`/`--plan`; \
                         pass at most one"
                            .to_string(),
                    );
                }
                tree = Some(PathBuf::from(value));
                mode = Some(Mode::Inspect);
            }
            "--target" => {
                let Some(value) = iter.next() else {
                    return Err(
                        "bundle: `--target` needs a value (windows, macos or linux)".to_string()
                    );
                };
                if platform.is_some() {
                    return Err("bundle: `--target` given more than once".to_string());
                }
                platform = Some(Platform::parse(value)?);
            }
            flag @ ("--check" | "--plan") => {
                let selected = if flag == "--check" {
                    Mode::Check
                } else {
                    Mode::Plan
                };
                match mode {
                    Some(existing) if existing != selected => {
                        return Err(
                            "bundle: `--check`, `--plan` and `--inspect` select different \
                             behaviours; pass at most one"
                                .to_string(),
                        );
                    }
                    _ => mode = Some(selected),
                }
            }
            other => {
                return Err(format!(
                    "bundle: unrecognised argument `{other}` (expected --check, --plan, \
                     --inspect <dir>, or --target <windows|macos|linux>)"
                ));
            }
        }
    }

    Ok(BundleArgs {
        platform: match platform {
            Some(platform) => platform,
            None => Platform::host()?,
        },
        mode: mode.unwrap_or(Mode::Materialise),
        tree,
    })
}

/// The layout as printed lines, one per entry, for `--plan` and for the header of a materialising
/// run. Returned rather than printed so a test can read it.
pub fn describe(layout: &Layout) -> Vec<String> {
    let mut lines = vec![format!(
        "bundle: {} layout -- {} file(s)",
        layout.platform.name(),
        layout.entries.len()
    )];
    lines.extend(layout.entries.iter().map(|entry| {
        let source = match &entry.source {
            Source::Build(name) => format!("build/{name}"),
            Source::Repo(name) => format!("repo/{name}"),
            Source::Generated(_) => "generated".to_string(),
        };
        format!("  - {} <- {source}", entry.dest)
    }));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic repository root and release build directory: a fake shared library for **every**
    /// platform (a few bytes, not a real dylib — nothing here loads one), a fake executable under
    /// both names, and the four staged documents. Returns `(repo_root, build_dir)`.
    fn synthetic_inputs(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("xtask-bundle-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let build = dir.join("target/release");
        std::fs::create_dir_all(&build).unwrap();

        for platform in Platform::ALL {
            std::fs::write(
                build.join(platform.clap_library()),
                format!("fake {} cdylib", platform.name()),
            )
            .unwrap();
            std::fs::write(build.join(platform.standalone()), b"fake standalone").unwrap();
        }
        for document in staged_documents() {
            std::fs::write(dir.join(document), format!("# {document}\n")).unwrap();
        }
        (dir, build)
    }

    /// FR-PKG-020, all three platforms, against **produced** output rather than against the plan
    /// that produced it: materialise a staging tree for each platform from a fake dylib, assert
    /// [`check`] finds nothing wrong with what actually landed on disk, assert the macOS tree is
    /// the bundle directory CLAP's `entry.h` defines and the other two are the renamed library, and
    /// assert the negative case the requirement exists for — a plain file named `Namir.clap` on
    /// macOS is reported as a violation.
    ///
    /// Runs on every host, on every runner, because [`plan`] never consults the host.
    ///
    /// **What the tag does and does not claim.** This test also asserts the macOS `Namir.app`
    /// bundle and its own negative case, added when the packaging lane found the standalone staged
    /// as a bare Mach-O. That is *not* part of FR-PKG-020, whose text is about "the CLAP artifact"
    /// and nothing else — the application bundle answers D-18.3's `/Applications` payload and
    /// [`MICROPHONE_USAGE_DESCRIPTION`]. The tag's claim is therefore unchanged and stays plain:
    /// what it asserts about FR-PKG-020 — every platform, produced output, the requirement's own
    /// `Verify: S` method — is exactly what it asserted before, and the extra assertions neither
    /// widen nor weaken it.
    // trace: FR-PKG-020
    #[test]
    fn every_platforms_produced_layout_is_the_form_its_loader_requires() {
        let (repo, build) = synthetic_inputs("form");

        for platform in Platform::ALL {
            let layout = plan(platform);
            let staging = repo.join("staging").join(platform.name());
            materialise(&repo, &build, &staging, &layout).unwrap();

            assert_eq!(
                check(&staging, &layout).unwrap(),
                Vec::<String>::new(),
                "{}: a freshly produced tree must be clean",
                platform.name()
            );

            let artifact = staging.join(CLAP_ARTIFACT);
            if platform.clap_is_a_bundle() {
                assert!(
                    artifact.is_dir(),
                    "macOS: {CLAP_ARTIFACT} must be a directory"
                );
                let contents = artifact.join("Contents");
                assert!(contents.join("Info.plist").is_file());
                assert!(contents.join("PkgInfo").is_file());
                assert!(
                    contents
                        .join("MacOS")
                        .join(platform.clap_library())
                        .is_file(),
                    "the dylib must be inside Contents/MacOS, not renamed at the top level"
                );
                assert_eq!(
                    std::fs::read(contents.join("PkgInfo")).unwrap(),
                    b"BNDL????",
                    "a plugin bundle's PkgInfo is exactly eight bytes, and its type is BNDL"
                );
                let plist = std::fs::read_to_string(contents.join("Info.plist")).unwrap();
                for key in [
                    "CFBundleExecutable",
                    "CFBundleIdentifier",
                    "CFBundlePackageType",
                    "CFBundleName",
                ] {
                    assert!(plist.contains(key), "Info.plist lacks {key}:\n{plist}");
                }
            } else {
                assert!(
                    artifact.is_file(),
                    "{}: {CLAP_ARTIFACT} must be the renamed shared library",
                    platform.name()
                );
                assert_eq!(
                    std::fs::read(&artifact).unwrap(),
                    std::fs::read(build.join(platform.clap_library())).unwrap(),
                    "{}: the renamed artifact must be the built library's bytes",
                    platform.name()
                );
            }

            // The standalone: an application bundle on macOS, a plain executable elsewhere.
            let app = staging.join(APP_ARTIFACT);
            if platform.clap_is_a_bundle() {
                assert!(app.is_dir(), "macOS: {APP_ARTIFACT} must be a directory");
                let contents = app.join("Contents");
                assert!(
                    contents.join("MacOS").join(platform.standalone()).is_file(),
                    "the executable must be inside Contents/MacOS"
                );
                assert!(
                    !staging.join(platform.standalone()).exists(),
                    "macOS stages no bare executable beside the bundle"
                );
                assert_eq!(
                    std::fs::read(contents.join("PkgInfo")).unwrap(),
                    b"APPL????",
                    "an application's PkgInfo is APPL, not the plugin's BNDL"
                );
                let plist = std::fs::read_to_string(contents.join("Info.plist")).unwrap();
                for key in [
                    "CFBundleExecutable",
                    "CFBundleIdentifier",
                    "CFBundleName",
                    "CFBundlePackageType",
                    "CFBundleShortVersionString",
                    "CFBundleVersion",
                    "LSMinimumSystemVersion",
                    "NSHighResolutionCapable",
                    "NSMicrophoneUsageDescription",
                ] {
                    assert!(plist.contains(key), "Info.plist lacks {key}:\n{plist}");
                }
                assert!(
                    plist.contains("<string>APPL</string>"),
                    "an application is APPL, not BNDL:\n{plist}"
                );
            } else {
                assert!(staging.join(platform.standalone()).is_file());
                assert!(
                    !app.exists(),
                    "{}: an application bundle has no meaning here",
                    platform.name()
                );
            }
        }

        // The negative case, and the whole reason FR-PKG-020 is a requirement: on macOS a renamed
        // dylib is not a plugin. Replace the produced bundle with exactly that and re-check.
        let macos = repo.join("staging/macos");
        std::fs::remove_dir_all(macos.join(CLAP_ARTIFACT)).unwrap();
        std::fs::write(macos.join(CLAP_ARTIFACT), b"fake macos cdylib").unwrap();
        let violations = check(&macos, &plan(Platform::MacOs)).unwrap();
        assert!(
            violations
                .iter()
                .any(|v| v.contains("must be a bundle directory") && v.contains("plain file")),
            "a renamed dylib on macOS must be reported as itself: {violations:#?}"
        );
        assert!(
            violations.iter().any(|v| v.contains("Info.plist")),
            "the bundle's absent members must be reported too: {violations:#?}"
        );

        // The mirror of that for the application bundle: a bare Mach-O named `Namir.app`. Not
        // FR-PKG-020's subject -- that requirement is about the CLAP artifact -- but the same
        // class of mistake, and the one that costs the user the audio input device.
        std::fs::remove_dir_all(macos.join(APP_ARTIFACT)).unwrap();
        std::fs::write(macos.join(APP_ARTIFACT), b"fake macos executable").unwrap();
        let violations = check(&macos, &plan(Platform::MacOs)).unwrap();
        assert!(
            violations.iter().any(|v| {
                v.contains("must be an application bundle")
                    && v.contains("NSMicrophoneUsageDescription")
            }),
            "a bare executable named Namir.app must be reported, with the reason: {violations:#?}"
        );
        assert!(
            violations
                .iter()
                .any(|v| v.contains(APP_ARTIFACT) && v.contains("PkgInfo")),
            "the app bundle's absent members must be reported too: {violations:#?}"
        );

        // And the mirror image: a *directory* named `Namir.clap` on Windows is equally wrong.
        let windows = repo.join("staging/windows");
        std::fs::remove_file(windows.join(CLAP_ARTIFACT)).unwrap();
        std::fs::create_dir_all(windows.join(CLAP_ARTIFACT)).unwrap();
        let violations = check(&windows, &plan(Platform::Windows)).unwrap();
        assert!(
            violations
                .iter()
                .any(|v| v.contains("renamed to `Namir.clap`") && v.contains("found a directory")),
            "{violations:#?}"
        );

        // And a `Namir.app` that wandered into a Windows tree is reported rather than ignored.
        std::fs::create_dir_all(windows.join(APP_ARTIFACT)).unwrap();
        let violations = check(&windows, &plan(Platform::Windows)).unwrap();
        assert!(
            violations
                .iter()
                .any(|v| v.contains(APP_ARTIFACT) && v.contains("no meaning on windows")),
            "{violations:#?}"
        );

        std::fs::remove_dir_all(&repo).ok();
    }

    /// FR-PKG-040's three files, in every platform's produced staging tree, asserted by the same
    /// [`check`] the packaging step runs — and asserted to *fail* when one of them is removed,
    /// since a presence check that passes on an empty tree asserts nothing.
    // trace-partial: FR-PKG-040
    // uncovered: FR-PKG-040 — the requirement quantifies over "every distribution, installer and
    // uncovered: archive alike", and this test spans none of them: its subject is a synthetic
    // uncovered: staging tree, which is what goes *into* a packager rather than what comes out.
    // uncovered: M14's bundle-and-inspect lane in ci.yml is what opens produced archives on all
    // uncovered: three platforms — this site stays partial because the artifact annotated here is
    // uncovered: this test, and it asserts nothing about any distribution; closes M8
    #[test]
    fn every_staged_tree_carries_the_attribution_file_and_both_licence_texts() {
        let (repo, build) = synthetic_inputs("licences");

        for platform in Platform::ALL {
            let layout = plan(platform);
            let staging = repo.join("staging").join(platform.name());
            materialise(&repo, &build, &staging, &layout).unwrap();

            for document in LICENCE_DOCUMENTS {
                assert!(
                    staging.join(document).is_file(),
                    "{}: {document} is not in the staging tree",
                    platform.name()
                );
            }
            assert!(check(&staging, &layout).unwrap().is_empty());

            // Removing any one of the three is a violation naming it.
            for document in LICENCE_DOCUMENTS {
                std::fs::remove_file(staging.join(document)).unwrap();
                let violations = check(&staging, &layout).unwrap();
                assert!(
                    violations.iter().any(|v| v.contains(document)),
                    "{}: removing {document} must be reported: {violations:#?}",
                    platform.name()
                );
                std::fs::write(staging.join(document), "restored").unwrap();
            }
        }

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn the_macos_layout_is_computed_on_whatever_host_this_runs_on() {
        // The property FR-PKG-020's test rests on, asserted directly: `plan` is pure, so the macOS
        // layout is available on Windows and Linux runners, not only on the macOS release leg.
        let layout = plan(Platform::MacOs);
        let dests: Vec<&str> = layout.entries.iter().map(|e| e.dest.as_str()).collect();
        assert_eq!(
            dests,
            vec![
                "Namir.clap/Contents/Info.plist",
                "Namir.clap/Contents/PkgInfo",
                "Namir.clap/Contents/MacOS/libnamir_clap.dylib",
                "Namir.app/Contents/Info.plist",
                "Namir.app/Contents/PkgInfo",
                "Namir.app/Contents/MacOS/namir",
                "THIRD-PARTY-NOTICES.md",
                "LICENSE-MIT",
                "LICENSE-APACHE",
                "README.md",
                "TRADEMARK.md",
            ]
        );
        assert_eq!(plan(Platform::MacOs), layout, "plan is deterministic");
    }

    #[test]
    fn the_windows_and_linux_layouts_rename_the_library_and_generate_nothing() {
        for (platform, library, executable) in [
            (Platform::Windows, "namir_clap.dll", "namir.exe"),
            (Platform::Linux, "libnamir_clap.so", "namir"),
        ] {
            let layout = plan(platform);
            assert_eq!(
                layout.entries[0],
                Entry {
                    dest: CLAP_ARTIFACT.to_string(),
                    source: Source::Build(library.to_string()),
                }
            );
            assert_eq!(layout.entries[1].dest, executable);
            assert!(
                !layout
                    .entries
                    .iter()
                    .any(|e| matches!(e.source, Source::Generated(_))),
                "only macOS generates anything"
            );
            assert!(
                !layout.entries.iter().any(|e| e.dest.contains(APP_ARTIFACT)),
                "{}: the standalone is a plain executable, not an application bundle",
                platform.name()
            );
        }
    }

    #[test]
    fn the_plugin_plist_declares_the_plugin_id_namir_clap_already_uses() {
        // One artifact, one reverse-DNS identity: this literal and `namir-clap`'s own `PLUGIN_ID`
        // are the same string, and this test is the reminder of that when either is edited.
        let plist = plugin_info_plist("libnamir_clap.dylib");
        assert!(
            plist.contains("<string>org.legrand.namir</string>"),
            "{plist}"
        );
        assert!(plist.contains("<string>BNDL</string>"), "{plist}");
        assert!(plist.contains("<string>Namir</string>"), "{plist}");
        assert!(
            plist.contains("<string>libnamir_clap.dylib</string>"),
            "CFBundleExecutable must name the dylib inside Contents/MacOS: {plist}"
        );
        assert!(plist.ends_with("</plist>\n"));
        assert_eq!(
            plugin_info_plist("libnamir_clap.dylib"),
            plist,
            "deterministic"
        );
        assert!(
            !plist.contains("NSMicrophoneUsageDescription"),
            "a plugin declares no usage description -- the host holds the grant:\n{plist}"
        );
    }

    /// The application bundle's own plist, key by key, because every one of the four keys the
    /// plugin's does not carry is there for a stated reason and a silent drop would be invisible
    /// until a user on macOS hit it.
    #[test]
    fn the_app_plist_carries_a_distinct_identifier_and_the_microphone_description() {
        let plist = app_info_plist("namir");

        // The identifier must differ from the plugin's: TCC keys the microphone grant on it.
        assert_ne!(APP_BUNDLE_IDENTIFIER, PLUGIN_BUNDLE_IDENTIFIER);
        assert!(
            plist.contains("<string>org.legrand.namir.standalone</string>"),
            "{plist}"
        );
        assert!(
            !plist.contains("<string>org.legrand.namir</string>"),
            "the app must not carry the plugin's identifier:\n{plist}"
        );

        // An application is APPL, never the plugin's BNDL.
        assert!(plist.contains("<string>APPL</string>"), "{plist}");
        assert!(!plist.contains("BNDL"), "{plist}");

        assert!(
            plist.contains("<key>NSMicrophoneUsageDescription</key>"),
            "{plist}"
        );
        assert!(plist.contains(MICROPHONE_USAGE_DESCRIPTION), "{plist}");
        assert!(
            plist.contains(&format!(
                "<key>LSMinimumSystemVersion</key>\n\t<string>{MINIMUM_MACOS_VERSION}</string>"
            )),
            "{plist}"
        );
        for key in [
            "CFBundleShortVersionString",
            "CFBundleVersion",
            "CFBundleInfoDictionaryVersion",
        ] {
            assert!(plist.contains(&format!("<key>{key}</key>")), "{plist}");
        }
        assert!(
            plist.contains("<key>NSHighResolutionCapable</key>\n\t<true/>"),
            "a boolean key, not a string: {plist}"
        );
        assert_eq!(app_info_plist("namir"), plist, "deterministic");
    }

    #[test]
    fn a_plist_emits_its_keys_in_sorted_order_whatever_order_it_was_given() {
        let entry = |key, value| (key, PlistValue::Text(value));
        let forward = plist(&[entry("Alpha", "1"), entry("Beta", "2"), entry("Gamma", "3")]);
        let shuffled = plist(&[entry("Gamma", "3"), entry("Alpha", "1"), entry("Beta", "2")]);
        assert_eq!(forward, shuffled, "key order must not reach the bytes");
        let alpha = forward.find("Alpha").unwrap();
        let beta = forward.find("Beta").unwrap();
        assert!(alpha < beta && beta < forward.find("Gamma").unwrap());
    }

    #[test]
    fn a_plist_escapes_the_three_characters_xml_reserves() {
        let escaped = plist(&[("K", PlistValue::Text("guitar & bass <live>"))]);
        assert!(
            escaped.contains("<string>guitar &amp; bass &lt;live&gt;</string>"),
            "{escaped}"
        );
    }

    #[test]
    fn a_boolean_is_a_plist_boolean_not_the_string_true() {
        // `<string>true</string>` is a different document from `<true/>`, and `plutil -lint` is
        // entitled to reject a boolean key carrying a string.
        let rendered = plist(&[("Yes", PlistValue::Boolean(true))]);
        assert!(
            rendered.contains("<key>Yes</key>\n\t<true/>\n"),
            "{rendered}"
        );
        assert!(!rendered.contains("<string>true</string>"), "{rendered}");
        assert!(
            plist(&[("No", PlistValue::Boolean(false))]).contains("<false/>"),
            "both arms render"
        );
    }

    #[test]
    fn the_two_pkginfo_files_differ_in_type_and_are_eight_bytes_each() {
        // Apple's definition: PkgInfo is the four-byte package type followed by the four-byte
        // creator code, carrying the same two values as CFBundlePackageType and CFBundleSignature.
        // A loadable bundle is BNDL; an application is APPL. `????` is the documented unset creator.
        assert_eq!(PLUGIN_PKG_INFO.len(), 8);
        assert_eq!(APP_PKG_INFO.len(), 8);
        assert_eq!(&PLUGIN_PKG_INFO[..4], "BNDL");
        assert_eq!(&APP_PKG_INFO[..4], "APPL");
        assert_eq!(&PLUGIN_PKG_INFO[4..], "????");
        assert_eq!(&APP_PKG_INFO[4..], "????");
        // And each agrees with the CFBundlePackageType its own plist declares.
        assert!(
            plugin_info_plist("x").contains(&format!("<string>{}</string>", &PLUGIN_PKG_INFO[..4]))
        );
        assert!(app_info_plist("x").contains(&format!("<string>{}</string>", &APP_PKG_INFO[..4])));
    }

    /// [`PRODUCT_VERSION`] is a duplicate of `namir-app`'s manifest version, held here so [`plan`]
    /// stays pure. A duplicate nothing checks is a duplicate that drifts, so check it.
    #[test]
    fn the_bundled_version_tracks_namir_apps_own() {
        let manifest = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("xtask/ has a parent")
                .join("crates/namir-app/Cargo.toml"),
        )
        .expect("namir-app's manifest is checked in");

        let declared = manifest
            .lines()
            .find_map(|line| line.strip_prefix("version = "))
            .map(|value| value.trim().trim_matches('"').to_string())
            .expect("namir-app declares a version");

        assert_eq!(
            declared, PRODUCT_VERSION,
            "CFBundleShortVersionString/CFBundleVersion must track crates/namir-app/Cargo.toml"
        );
    }

    /// What `--inspect` is pointed at in CI is not a staging tree: it is a directory an archive was
    /// unpacked into, which carries whatever the packaging step added on the way — `install.sh` and
    /// `INSTALL.md` on Linux, and the `__MACOSX` sidecar `ditto` can leave behind. Those must not
    /// read as violations, or the lane would be red on every run for the wrong reason; and a
    /// licence text the archive *lost* must still be reported by name, or the lane asserts nothing.
    ///
    /// This is the check M14's `bundle-and-inspect` job runs against every produced archive, driven
    /// here against the tree shape that job hands it.
    #[test]
    fn an_unpacked_archive_may_carry_extra_files_but_not_lose_a_required_one() {
        let (repo, build) = synthetic_inputs("unpacked");
        let layout = plan(Platform::Linux);
        let unpacked = repo.join("package/namir-0.0.0-linux-x86_64");
        materialise(&repo, &build, &unpacked, &layout).unwrap();

        std::fs::write(unpacked.join("install.sh"), "#!/bin/sh\n").unwrap();
        std::fs::write(unpacked.join("INSTALL.md"), "# Installing\n").unwrap();
        std::fs::create_dir_all(unpacked.join("__MACOSX")).unwrap();
        assert!(
            check(&unpacked, &layout).unwrap().is_empty(),
            "an archive's own additions are not deviations from the layout"
        );

        for document in LICENCE_DOCUMENTS {
            std::fs::remove_file(unpacked.join(document)).unwrap();
            let violations = check(&unpacked, &layout).unwrap();
            assert!(
                violations.iter().any(|v| v.contains(document)),
                "an archive that lost {document} must be reported: {violations:#?}"
            );
            std::fs::write(unpacked.join(document), "restored").unwrap();
        }

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn a_hand_edited_generated_file_is_a_violation() {
        let (repo, build) = synthetic_inputs("hand-edited");
        let layout = plan(Platform::MacOs);
        let staging = repo.join("staging/macos");
        materialise(&repo, &build, &staging, &layout).unwrap();

        std::fs::write(
            staging.join("Namir.clap/Contents/Info.plist"),
            "<plist>edited by hand</plist>\n",
        )
        .unwrap();
        let violations = check(&staging, &layout).unwrap();
        assert_eq!(violations.len(), 1, "{violations:#?}");
        assert!(violations[0].contains("hand-edited"), "{}", violations[0]);
        assert!(violations[0].contains(BUNDLE_REMEDY), "{}", violations[0]);

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn materialising_clears_a_stale_tree_rather_than_updating_it() {
        let (repo, build) = synthetic_inputs("stale");
        let layout = plan(Platform::Windows);
        let staging = repo.join("staging/windows");
        materialise(&repo, &build, &staging, &layout).unwrap();
        std::fs::write(staging.join("leftover-from-an-older-release.dll"), b"x").unwrap();

        materialise(&repo, &build, &staging, &layout).unwrap();
        assert!(!staging.join("leftover-from-an-older-release.dll").exists());
        assert!(check(&staging, &layout).unwrap().is_empty());

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn a_missing_build_artifact_names_the_file_it_could_not_find() {
        let (repo, build) = synthetic_inputs("missing-artifact");
        std::fs::remove_file(build.join("libnamir_clap.dylib")).unwrap();

        let err = materialise(
            &repo,
            &build,
            &repo.join("staging/macos"),
            &plan(Platform::MacOs),
        )
        .unwrap_err();
        assert!(err.contains("libnamir_clap.dylib"), "{err}");

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn an_absent_staging_tree_is_an_error_not_a_pile_of_violations() {
        let dir = std::env::temp_dir().join(format!("xtask-bundle-absent-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let err = check(&dir, &plan(Platform::Linux)).unwrap_err();
        assert!(err.contains("no staging tree"), "{err}");
        assert!(err.contains(BUNDLE_REMEDY), "{err}");
    }

    #[test]
    fn parse_args_is_strict() {
        let args = |list: &[&str]| -> Vec<String> { list.iter().map(|s| s.to_string()).collect() };

        assert_eq!(
            parse_args(&args(&["--target", "macos"])).unwrap(),
            BundleArgs {
                platform: Platform::MacOs,
                mode: Mode::Materialise,
                tree: None,
            }
        );
        assert_eq!(
            parse_args(&args(&["--check", "--target", "linux"])).unwrap(),
            BundleArgs {
                platform: Platform::Linux,
                mode: Mode::Check,
                tree: None,
            }
        );
        assert_eq!(parse_args(&args(&["--plan"])).unwrap().mode, Mode::Plan);

        // M14: `--inspect` takes the directory a produced distribution was unpacked into, and
        // selects a mode of its own rather than riding on `--check`.
        let inspect = parse_args(&args(&[
            "--inspect",
            "dist/unpacked",
            "--target",
            "windows",
        ]))
        .expect("--inspect <dir> parses");
        assert_eq!(
            inspect,
            BundleArgs {
                platform: Platform::Windows,
                mode: Mode::Inspect,
                tree: Some(PathBuf::from("dist/unpacked")),
            }
        );

        // A typo must not silently select the default behaviour.
        for bad in [
            vec!["--wrote"],
            vec!["--check=true"],
            vec!["bundle"],
            vec!["--target"],
            vec!["--target", "freebsd"],
            vec!["--check", "--plan"],
            vec!["--target", "macos", "--target", "linux"],
            vec!["--inspect"],
            vec!["--inspect", "a", "--inspect", "b"],
            vec!["--inspect", "a", "--check"],
            vec!["--plan", "--inspect", "a"],
        ] {
            assert!(
                parse_args(&args(&bad)).is_err(),
                "{bad:?} must be refused outright"
            );
        }
    }

    #[test]
    fn describe_lists_every_entry_and_where_it_comes_from() {
        let lines = describe(&plan(Platform::MacOs));
        assert_eq!(lines.len(), 1 + plan(Platform::MacOs).entries.len());
        assert!(lines[0].contains("macos"));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("PkgInfo") && l.contains("generated"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("THIRD-PARTY-NOTICES.md") && l.contains("repo/"))
        );
    }
}
