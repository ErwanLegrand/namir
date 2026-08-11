//! `cargo run -p xtask -- bundle [--target <windows|macos|linux>] [--check | --plan]`: M13's first
//! deliverable and the primitive D-18.3's release pipeline is built on — **build → `xtask bundle`
//! → per-OS package → GitHub Release**, in that order, with everything after the arrow depending
//! on this step.
//!
//! It assembles a per-platform *staging tree* from artifacts a release build has already produced:
//! the CLAP artifact in the form that platform's loader requires, the standalone executable, and
//! the three documents FR-PKG-040 requires every distribution to carry (plus `README.md`). It
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
//! is the precedent). A plist is XML with a fixed shape and four keys here, so [`info_plist`]
//! writes it directly and deterministically, in sorted-key order, rather than taking a plist crate
//! for it. `PkgInfo` is eight literal bytes.
//!
//! # Argument parsing is strict
//!
//! Like `traceability` and `nam-parity`, and unlike the lenient `any(|a| a == "--write")` the
//! generate-and-diff subcommands use (`main.rs`'s own note on the house rule): the flags here
//! select between *behaviours* — materialise, assert, or describe — and between *target
//! platforms*, so a typo that silently selected a different one of those would be worse than a
//! loud refusal. Anything unrecognised exits 2.

use std::path::{Path, PathBuf};

/// The bundle identifier the macOS `Info.plist` declares.
///
/// Deliberately the **same string** `namir-clap` already declares as its CLAP plugin id
/// (`crates/namir-clap/src/lib.rs`'s `PLUGIN_ID`). One product, one reverse-DNS identity; inventing
/// a second here would mean a host and the operating system disagreeing about what this artifact
/// is. It is a second copy of the literal rather than an import because `xtask` is in neither
/// shipped product's dependency graph — the same reason `identity.rs` re-states `MARK_FILL`.
pub const BUNDLE_IDENTIFIER: &str = "org.legrand.namir";

/// `CFBundleName`, and the stem of the artifact every platform's loader looks for.
pub const BUNDLE_NAME: &str = "Namir";

/// The CLAP artifact's name in every staging tree: a file on Windows and Linux, a bundle directory
/// on macOS (see this module's header).
pub const CLAP_ARTIFACT: &str = "Namir.clap";

/// The exact contents of a macOS bundle's `PkgInfo`: an eight-byte type/creator pair, the type
/// being `BNDL` and the creator unset (`????`). No trailing newline — the file is eight bytes.
pub const PKG_INFO: &str = "BNDL????";

/// FR-PKG-040's enumerated set, exactly: NFR-LIC-030's machine-generated attribution file, and the
/// full text of both licences of NFR-LIC-010. Named as its own constant so the requirement's set is
/// legible as itself rather than as a prefix of the longer list [`staged_documents`] returns.
pub const LICENCE_DOCUMENTS: [&str; 3] =
    ["THIRD-PARTY-NOTICES.md", "LICENSE-MIT", "LICENSE-APACHE"];

/// Staged beside [`LICENCE_DOCUMENTS`] though FR-PKG-040 does not ask for it: a distribution with
/// no statement of what it is is a worse artifact than one with a redundant file in it.
pub const README: &str = "README.md";

/// Every document a staging tree carries, in staging order.
pub fn staged_documents() -> [&'static str; 4] {
    let [notices, mit, apache] = LICENCE_DOCUMENTS;
    [notices, mit, apache, README]
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

    if platform.clap_is_a_bundle() {
        entries.push(Entry {
            dest: format!("{CLAP_ARTIFACT}/Contents/Info.plist"),
            source: Source::Generated(info_plist(library)),
        });
        entries.push(Entry {
            dest: format!("{CLAP_ARTIFACT}/Contents/PkgInfo"),
            source: Source::Generated(PKG_INFO.to_string()),
        });
        entries.push(Entry {
            dest: format!("{CLAP_ARTIFACT}/Contents/MacOS/{library}"),
            source: Source::Build(library.to_string()),
        });
    } else {
        entries.push(Entry {
            dest: CLAP_ARTIFACT.to_string(),
            source: Source::Build(library.to_string()),
        });
    }

    entries.push(Entry {
        dest: platform.standalone().to_string(),
        source: Source::Build(platform.standalone().to_string()),
    });
    for document in staged_documents() {
        entries.push(Entry {
            dest: document.to_string(),
            source: Source::Repo(document.to_string()),
        });
    }

    Layout { platform, entries }
}

/// The macOS bundle's `Info.plist`, as deterministic text.
///
/// Four keys, which is what `docs/user-guide.md` specifies as the minimum and what a loader
/// actually reads: `CFBundleExecutable` (the dylib inside `Contents/MacOS`), `CFBundleIdentifier`,
/// `CFBundleName` and `CFBundlePackageType` (`BNDL`). Emitted in sorted-key order with tab
/// indentation — `plutil`'s own output convention — so that two runs, on two machines, produce
/// identical bytes and [`check`] can byte-compare rather than parse.
///
/// The `DOCTYPE`'s URL is a public identifier in a fixed string, resolved by nothing at build or
/// run time; it is what every `Info.plist` carries and no network access of any kind.
pub fn info_plist(executable: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>CFBundleExecutable</key>\n\
         \t<string>{executable}</string>\n\
         \t<key>CFBundleIdentifier</key>\n\
         \t<string>{BUNDLE_IDENTIFIER}</string>\n\
         \t<key>CFBundleName</key>\n\
         \t<string>{BUNDLE_NAME}</string>\n\
         \t<key>CFBundlePackageType</key>\n\
         \t<string>BNDL</string>\n\
         </dict>\n\
         </plist>\n"
    )
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BundleArgs {
    pub platform: Platform,
    pub mode: Mode,
}

/// Parses `bundle`'s own argument list (everything after the `bundle` token), strictly: see this
/// module's header for why an unrecognised flag here is refused rather than ignored.
pub fn parse_args(args: &[String]) -> Result<BundleArgs, String> {
    let mut platform = None;
    let mut mode = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
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
                            "bundle: `--check` and `--plan` select different behaviours; pass at \
                             most one"
                                .to_string(),
                        );
                    }
                    _ => mode = Some(selected),
                }
            }
            other => {
                return Err(format!(
                    "bundle: unrecognised argument `{other}` (expected --check, --plan, or \
                     --target <windows|macos|linux>)"
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
                    "PkgInfo is exactly eight bytes"
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

            assert!(staging.join(platform.standalone()).is_file());
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

        std::fs::remove_dir_all(&repo).ok();
    }

    /// FR-PKG-040's three files, in every platform's produced staging tree, asserted by the same
    /// [`check`] the packaging step runs — and asserted to *fail* when one of them is removed,
    /// since a presence check that passes on an empty tree asserts nothing.
    // trace-partial: FR-PKG-040
    // uncovered: FR-PKG-040 — only the staging tree is spanned. The requirement quantifies over
    // uncovered: "every distribution, installer and archive alike", and the Windows installer, the
    // uncovered: macOS .pkg/.dmg and the plain archives are later deliverables of this milestone
    // uncovered: that do not exist yet, so nothing asserts the three files inside a produced
    // uncovered: distribution; closes M13
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
                "namir",
                "THIRD-PARTY-NOTICES.md",
                "LICENSE-MIT",
                "LICENSE-APACHE",
                "README.md",
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
        }
    }

    #[test]
    fn the_plist_declares_the_plugin_id_namir_clap_already_uses() {
        // One product, one reverse-DNS identity: this literal and `namir-clap`'s own `PLUGIN_ID`
        // are the same string, and this test is the reminder of that when either is edited.
        let plist = info_plist("libnamir_clap.dylib");
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
        assert_eq!(info_plist("libnamir_clap.dylib"), plist, "deterministic");
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
            }
        );
        assert_eq!(
            parse_args(&args(&["--check", "--target", "linux"])).unwrap(),
            BundleArgs {
                platform: Platform::Linux,
                mode: Mode::Check,
            }
        );
        assert_eq!(parse_args(&args(&["--plan"])).unwrap().mode, Mode::Plan);

        // A typo must not silently select the default behaviour.
        for bad in [
            vec!["--wrote"],
            vec!["--check=true"],
            vec!["bundle"],
            vec!["--target"],
            vec!["--target", "freebsd"],
            vec!["--check", "--plan"],
            vec!["--target", "macos", "--target", "linux"],
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
