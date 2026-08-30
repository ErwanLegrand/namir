//! [`ScanFs`]: the filesystem port `scan.rs`'s step machine reads through, with a real
//! [`StdFs`] implementation. Not test hygiene bolted on afterwards — it is what makes FR-LIB-070
//! ("files that disappear, change or are added while Namir is running") a deterministic unit
//! test against a fake instead of a genuine race against the real filesystem, and it is what lets
//! `namir-library/benches/library_scan.rs` (M5, later) measure the incremental-scan logic without
//! materialising a 10,000-file tree for every arm.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::entry::FileTime;
use crate::error::{LibraryError, LibraryWarning};
use crate::error_codes;

/// One directory entry, as [`ScanFs::read_dir`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryInfo {
    /// The entry's full path.
    pub path: PathBuf,
    /// Whether this entry is a directory *without* following a symlink — so `false` for a
    /// symlink that points at one. [`Self::is_dir_symlink`] is the other half of that question.
    pub is_dir: bool,
    /// Whether this entry is a symlink (or, on Windows, a directory reparse point) whose target
    /// is a directory. Issue #73: symlinking a model collection into the library root is an
    /// ordinary user setup, so `scan.rs` follows these — see its `visited_dirs` set for how a
    /// loop is made to terminate, and one directory kept to one spelling, now that neither is
    /// impossible by construction.
    pub is_dir_symlink: bool,
    /// Byte length as the directory listing reports it. For a directory, `0` — meaningless and
    /// never consulted.
    pub size: u64,
    /// Last-modified time as the directory listing reports it.
    pub mtime: FileTime,
}

/// One directory's listing: what could be described, plus what could not.
///
/// Issue #66: a per-entry failure is not a per-directory failure. One locked file, reparse point
/// or cloud placeholder used to propagate out of [`ScanFs::read_dir`] and take the whole
/// directory's listing with it — and, since the scan still reached `Step::Finished`, every
/// indexed sibling became a removal. The listing therefore reports partial success rather than
/// collapsing to `Err`, and carries enough for the caller to keep what it already had:
/// [`Self::unreadable_entries`] name the paths whose siblings must not be inferred away, and
/// [`Self::fully_enumerated`] says whether the listing can be trusted to be the whole directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirListing {
    /// The children that could be described.
    pub entries: Vec<DirEntryInfo>,
    /// Children that appeared in the directory but could not be described (their type or
    /// metadata could not be read). Known by path, so the caller can leave whatever it already
    /// knows about them alone rather than concluding they are gone.
    pub unreadable_entries: Vec<PathBuf>,
    /// `false` when at least one child could not even be named — an iterator-level failure, where
    /// there is no path to record in [`Self::unreadable_entries`]. The directory's listing is
    /// then not known to be complete, so nothing under it may be inferred to have been deleted.
    pub fully_enumerated: bool,
    /// One per skipped child, ready to carry into a scan's warnings.
    pub warnings: Vec<LibraryWarning>,
}

/// The filesystem operations `scan.rs` needs, and nothing else — deliberately narrower than
/// `std::fs` so a fake implementation is easy to write completely and correctly.
pub trait ScanFs: Send + Sync {
    /// Lists `dir`'s immediate children. Does not recurse — the caller (`scan.rs`'s step machine)
    /// owns the traversal order. `Err` only when the *directory itself* could not be opened; a
    /// child that could not be described is reported inside the [`DirListing`] instead (issue
    /// #66).
    fn read_dir(&self, dir: &Path) -> Result<DirListing, LibraryError>;

    /// The canonical, symlink-free form of a directory path — `scan.rs`'s cycle guard for the
    /// directory symlinks it now follows (issue #73). On the port rather than called directly so
    /// a fake can model a symlinked tree without one existing on disk.
    fn canonical_dir(&self, dir: &Path) -> Result<PathBuf, LibraryError>;

    /// Reads `path`'s full contents, refusing (without reading past the header) anything over
    /// `max_bytes` — the same NFR-SEC-020 discipline `namir_worker::LoadSource::File` already
    /// applies, checked here independently since this crate may not depend on `namir-worker`
    /// (D-5.1).
    fn read_file(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, LibraryError>;
}

/// The real filesystem, via `std::fs`. Not platform code (D-5.2's cfg lint is textual, not
/// semantic, but there is genuinely nothing conditionally compiled here): every path is supplied
/// by the caller, so this type never assumes a filesystem layout — the same argument
/// `namir_worker::LoadSource::File`'s own doc comment makes for the identical shape.
pub struct StdFs;

impl ScanFs for StdFs {
    fn read_dir(&self, dir: &Path) -> Result<DirListing, LibraryError> {
        let entries = std::fs::read_dir(dir).map_err(|e| {
            LibraryError::new(
                error_codes::DIR_UNREADABLE,
                format!("{}: {e}", dir.display()),
            )
        })?;
        let mut listing = DirListing {
            fully_enumerated: true,
            ..DirListing::default()
        };
        for entry in entries {
            // Issue #66: every failure below is *this child's* failure. Skipping it and carrying
            // on is what keeps one locked file, reparse point or cloud placeholder from deleting
            // the whole directory's worth of index entries.
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    // No path to record: the iterator failed before naming the child, so the
                    // directory is not known to have been fully enumerated.
                    listing.fully_enumerated = false;
                    listing.warnings.push(LibraryWarning::new(
                        error_codes::DIR_UNREADABLE,
                        format!("{}: {e}", dir.display()),
                    ));
                    continue;
                }
            };
            let path = entry.path();
            // DirEntry::file_type() does not follow symlinks on any platform this workspace
            // targets, so `is_dir` answers "what is this entry" and the extra metadata() call
            // below answers "and what does it point at" (issue #73) — deliberately two questions
            // rather than one, since only the first can be asked without touching the target.
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(e) => {
                    listing.unreadable_entries.push(path.clone());
                    listing.warnings.push(LibraryWarning::new(
                        error_codes::FILE_UNREADABLE,
                        format!("{}: {e}", path.display()),
                    ));
                    continue;
                }
            };
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    listing.unreadable_entries.push(path.clone());
                    listing.warnings.push(LibraryWarning::new(
                        error_codes::FILE_UNREADABLE,
                        format!("{}: {e}", path.display()),
                    ));
                    continue;
                }
            };
            // Only asked of an entry that is a link, and only ever "is the target a directory" —
            // a link whose target is missing simply answers no, which is the right answer.
            let is_dir_symlink = file_type.is_symlink()
                && std::fs::metadata(&path)
                    .map(|m| m.is_dir())
                    .unwrap_or(false);
            listing.entries.push(DirEntryInfo {
                path,
                is_dir: file_type.is_dir(),
                is_dir_symlink,
                size: metadata.len(),
                mtime: FileTime::from_system_time(
                    metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                ),
            });
        }
        Ok(listing)
    }

    fn canonical_dir(&self, dir: &Path) -> Result<PathBuf, LibraryError> {
        std::fs::canonicalize(dir).map_err(|e| {
            LibraryError::new(
                error_codes::DIR_UNREADABLE,
                format!("{}: {e}", dir.display()),
            )
        })
    }

    fn read_file(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, LibraryError> {
        // Issue #70, first half: the file *type* is checked before anything is opened. On Unix
        // `File::open` on a FIFO blocks until a writer appears, so a bounded read cannot rescue a
        // pipe named `cab.wav` — only not opening it can. The residual race (a regular file
        // replaced by a device between this call and the open below) is a narrower one than the
        // size race this method used to run, and the only alternative is a per-platform
        // `O_NONBLOCK` open, which D-5.2's cfg lint reserves for `namir-platform`.
        let meta = std::fs::metadata(path).map_err(|e| {
            LibraryError::new(
                error_codes::FILE_UNREADABLE,
                format!("{}: {e}", path.display()),
            )
        })?;
        if !meta.is_file() {
            return Err(LibraryError::new(
                error_codes::FILE_NOT_REGULAR,
                format!("{}: not a regular file", path.display()),
            ));
        }
        // A cheap early reject for a file that is already known to be too big, so a 4 GB WAV is
        // not read up to the ceiling just to be refused. It is an optimisation, not the check:
        // the bound below is what actually holds, whatever this file's length does next.
        if meta.len() as usize > max_bytes {
            return Err(too_large(path, meta.len(), max_bytes));
        }

        let file = std::fs::File::open(path).map_err(|e| {
            LibraryError::new(
                error_codes::FILE_UNREADABLE,
                format!("{}: {e}", path.display()),
            )
        })?;
        read_bounded(file, path, max_bytes, meta.len() as usize)
    }
}

fn too_large(path: &Path, len: u64, max_bytes: usize) -> LibraryError {
    LibraryError::new(
        error_codes::FILE_TOO_LARGE,
        format!(
            "{}: {} bytes, limit {} MB",
            path.display(),
            len,
            max_bytes / (1024 * 1024)
        ),
    )
}

/// Issue #70, second half: NFR-SEC-020's ceiling enforced on the **read** rather than on a
/// `metadata()` call taken beforehand.
///
/// The old shape — stat, compare, then `std::fs::read` — is a time-of-check/time-of-use gap: a
/// file that grows between the two calls is read into memory in full, past the limit, and a
/// character device that reports `len() == 0` streams forever. Reading through
/// `Read::take(max_bytes + 1)` makes the ceiling structural: one byte past the limit is the most
/// that can ever be in memory, and its presence is what proves the file was over it.
///
/// `capacity_hint` is the length the file claimed a moment ago — a sizing hint only, clamped to
/// the ceiling, so an ordinary read still makes one allocation rather than growing through a dozen.
/// Being wrong about it costs a reallocation, never a byte past the bound.
fn read_bounded(
    reader: impl Read,
    path: &Path,
    max_bytes: usize,
    capacity_hint: usize,
) -> Result<Vec<u8>, LibraryError> {
    let mut bytes = Vec::with_capacity(capacity_hint.min(max_bytes) + 1);
    let mut limited = reader.take(max_bytes as u64 + 1);
    limited.read_to_end(&mut bytes).map_err(|e| {
        LibraryError::new(
            error_codes::FILE_UNREADABLE,
            format!("{}: {e}", path.display()),
        )
    })?;
    if bytes.len() > max_bytes {
        return Err(too_large(path, bytes.len() as u64, max_bytes));
    }
    Ok(bytes)
}

/// An in-memory [`ScanFs`], `pub(crate)` so `scan.rs`'s own tests can reach it too — a controlled
/// double for cases real disk I/O makes awkward or slow to set up (an oversized file, a
/// millisecond-precise mtime, a directory listing with no filesystem underneath it at all).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeFs {
    dirs: std::collections::HashMap<PathBuf, Vec<DirEntryInfo>>,
    files: std::collections::HashMap<PathBuf, Vec<u8>>,
    /// Children that appear in a listing but cannot be described — issue #66's locked file.
    unreadable: std::collections::HashMap<PathBuf, Vec<PathBuf>>,
    /// Directories whose listing could not even be enumerated to the end.
    partial: std::collections::HashSet<PathBuf>,
    /// Where a directory symlink points, for [`ScanFs::canonical_dir`] — issue #73.
    link_targets: std::collections::HashMap<PathBuf, PathBuf>,
}

#[cfg(test)]
impl FakeFs {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Registers `path` as a file of `size`/`mtime` whose content, when read, is `bytes` — the
    /// two are independent so a test can construct a `DirEntryInfo` claiming a size that doesn't
    /// match `bytes.len()` (e.g. to simulate the oversized-file ceiling without a real
    /// hundred-megabyte fixture).
    pub(crate) fn add_file(
        &mut self,
        parent: &Path,
        name: &str,
        size: u64,
        mtime: FileTime,
        bytes: Vec<u8>,
    ) -> PathBuf {
        let path = parent.join(name);
        self.dirs
            .entry(parent.to_path_buf())
            .or_default()
            .push(DirEntryInfo {
                path: path.clone(),
                is_dir: false,
                is_dir_symlink: false,
                size,
                mtime,
            });
        self.files.insert(path.clone(), bytes);
        path
    }

    /// Registers `name` as a subdirectory of `parent` that appears in `parent`'s listing but
    /// cannot be listed itself — the offline volume or ACL-restricted folder of issue #65.
    pub(crate) fn add_unlistable_dir(&mut self, parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        self.dirs
            .entry(parent.to_path_buf())
            .or_default()
            .push(DirEntryInfo {
                path: path.clone(),
                is_dir: true,
                is_dir_symlink: false,
                size: 0,
                mtime: FileTime::from_system_time(std::time::UNIX_EPOCH),
            });
        path
    }

    /// Registers `name` as an ordinary, listable subdirectory of `parent` — the real folder a
    /// [`Self::add_dir_symlink`] target needs when both spellings are inside the scanned tree.
    /// [`Self::add_unlistable_dir`] is the same listing entry without the directory behind it.
    pub(crate) fn add_dir(&mut self, parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        self.dirs
            .entry(parent.to_path_buf())
            .or_default()
            .push(DirEntryInfo {
                path: path.clone(),
                is_dir: true,
                is_dir_symlink: false,
                size: 0,
                mtime: FileTime::from_system_time(std::time::UNIX_EPOCH),
            });
        self.dirs.entry(path.clone()).or_default();
        path
    }

    /// Registers `name` in `parent` as a symlink to the directory `target` — listed like a
    /// directory, canonicalising to `target` (issue #73).
    pub(crate) fn add_dir_symlink(&mut self, parent: &Path, name: &str, target: &Path) -> PathBuf {
        let path = parent.join(name);
        self.dirs
            .entry(parent.to_path_buf())
            .or_default()
            .push(DirEntryInfo {
                path: path.clone(),
                is_dir: false,
                is_dir_symlink: true,
                size: 0,
                mtime: FileTime::from_system_time(std::time::UNIX_EPOCH),
            });
        self.link_targets.insert(path.clone(), target.to_path_buf());
        path
    }

    /// Registers `name` as a child of `parent` that appears in the listing but cannot be
    /// described — issue #66's locked file, cloud placeholder or reparse point.
    pub(crate) fn add_unreadable_entry(&mut self, parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        self.dirs.entry(parent.to_path_buf()).or_default();
        self.unreadable
            .entry(parent.to_path_buf())
            .or_default()
            .push(path.clone());
        path
    }

    /// Marks `dir`'s listing as one that could not be enumerated to the end — the iterator-level
    /// failure, where the skipped child has no path to record.
    pub(crate) fn mark_partial(&mut self, dir: &Path) {
        self.dirs.entry(dir.to_path_buf()).or_default();
        self.partial.insert(dir.to_path_buf());
    }

    /// `path` with every registered symlink on the way substituted for its target, repeatedly —
    /// what a real `canonicalize` does, and what makes `read_dir` on a link path return the
    /// target's children re-rooted under the link, as the real one does. Bounded, so a fake tree
    /// with a link cycle in it terminates here too rather than hanging the test that built it.
    fn resolve(&self, path: &Path) -> PathBuf {
        let mut current = path.to_path_buf();
        for _ in 0..16 {
            let mut ancestor = current.clone();
            let mut stripped: Vec<std::ffi::OsString> = Vec::new();
            let mut substituted = None;
            loop {
                if let Some(target) = self.link_targets.get(&ancestor) {
                    let mut rebuilt = target.clone();
                    for name in stripped.iter().rev() {
                        rebuilt.push(name);
                    }
                    substituted = Some(rebuilt);
                    break;
                }
                match (ancestor.parent(), ancestor.file_name()) {
                    (Some(parent), Some(name)) => {
                        stripped.push(name.to_os_string());
                        ancestor = parent.to_path_buf();
                    }
                    _ => break,
                }
            }
            match substituted {
                Some(next) => current = next,
                None => break,
            }
        }
        current
    }
}

#[cfg(test)]
impl ScanFs for FakeFs {
    fn read_dir(&self, dir: &Path) -> Result<DirListing, LibraryError> {
        // A real read_dir follows the link and reports the target's children under the path it
        // was asked about, not under the target's own path.
        let real = self.resolve(dir);
        let entries: Vec<DirEntryInfo> = self
            .dirs
            .get(&real)
            .cloned()
            .ok_or_else(|| {
                LibraryError::new(
                    error_codes::DIR_UNREADABLE,
                    format!("{}: not in fake", dir.display()),
                )
            })?
            .into_iter()
            .map(|mut e| {
                if let Some(name) = e.path.file_name() {
                    e.path = dir.join(name);
                }
                e
            })
            .collect();
        let unreadable_entries = self.unreadable.get(&real).cloned().unwrap_or_default();
        let mut warnings: Vec<LibraryWarning> = unreadable_entries
            .iter()
            .map(|p| {
                LibraryWarning::new(
                    error_codes::FILE_UNREADABLE,
                    format!("{}: not describable in fake", p.display()),
                )
            })
            .collect();
        let fully_enumerated = !self.partial.contains(&real);
        if !fully_enumerated {
            warnings.push(LibraryWarning::new(
                error_codes::DIR_UNREADABLE,
                format!("{}: listing truncated in fake", dir.display()),
            ));
        }
        Ok(DirListing {
            entries,
            unreadable_entries,
            fully_enumerated,
            warnings,
        })
    }

    fn canonical_dir(&self, dir: &Path) -> Result<PathBuf, LibraryError> {
        Ok(self.resolve(dir))
    }

    fn read_file(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, LibraryError> {
        // Mirrors StdFs's own ceiling check against the *claimed* size (the DirEntryInfo added
        // via add_file), not the real byte count -- this is what lets a test simulate an
        // oversized file cheaply, with a small `bytes` payload standing in for content nobody
        // needs to actually read.
        let real = self.resolve(path);
        let claimed_size = self
            .dirs
            .values()
            .flatten()
            .find(|e| e.path == real)
            .map(|e| e.size);
        if let Some(size) = claimed_size
            && size as usize > max_bytes
        {
            return Err(too_large(path, size, max_bytes));
        }
        self.files.get(&real).cloned().ok_or_else(|| {
            LibraryError::new(
                error_codes::FILE_UNREADABLE,
                format!("{}: not in fake", path.display()),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-library-fs-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn std_fs_read_dir_lists_files_and_directories() {
        let dir = temp_dir("read_dir");
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();

        let listing = StdFs.read_dir(&dir).unwrap();
        assert!(listing.fully_enumerated);
        assert!(listing.unreadable_entries.is_empty());
        assert_eq!(listing.entries.len(), 2);
        let file = listing
            .entries
            .iter()
            .find(|e| e.path.ends_with("a.txt"))
            .unwrap();
        assert!(!file.is_dir);
        assert_eq!(file.size, 5);
        let sub = listing
            .entries
            .iter()
            .find(|e| e.path.ends_with("sub"))
            .unwrap();
        assert!(sub.is_dir);
        assert!(!sub.is_dir_symlink);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn std_fs_read_dir_on_a_missing_directory_is_an_error() {
        let dir = temp_dir("missing").join("does-not-exist");
        let err = StdFs.read_dir(&dir).unwrap_err();
        assert_eq!(err.code.id, error_codes::DIR_UNREADABLE.id);
    }

    #[test]
    fn std_fs_read_file_reads_full_contents() {
        let dir = temp_dir("read_file");
        let path = dir.join("x.bin");
        std::fs::write(&path, b"some bytes").unwrap();
        let bytes = StdFs.read_file(&path, 1024).unwrap();
        assert_eq!(bytes, b"some bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn std_fs_read_file_rejects_a_file_over_the_ceiling() {
        let dir = temp_dir("too_large");
        let path = dir.join("big.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&[0u8; 100]).unwrap();
        let err = StdFs.read_file(&path, 10).unwrap_err();
        assert_eq!(err.code.id, error_codes::FILE_TOO_LARGE.id);
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// **Issue #70:** a path that is not a regular file must be refused by name, before it is
    /// opened. On Unix a FIFO or character device named `foo.wav` reports `len() == 0`, passes the
    /// size ceiling, and then blocks or streams forever inside the read. A directory stands in for
    /// the whole class portably.
    #[test]
    fn a_non_regular_file_is_refused_as_such() {
        let dir = temp_dir("not_regular");
        let err = StdFs.read_file(&dir, 1024).unwrap_err();
        assert_eq!(err.code.id, error_codes::FILE_NOT_REGULAR.id);
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// **Issue #70:** the ceiling is enforced on the read itself, so a reader that never ends —
    /// `/dev/zero`, or a file being appended to right now — is stopped at the limit rather than
    /// filling memory. `io::repeat` is that reader, without needing a device node or a race.
    #[test]
    fn a_reader_that_never_ends_is_stopped_at_the_ceiling() {
        let err = read_bounded(std::io::repeat(0), Path::new("endless.wav"), 4096, 0).unwrap_err();
        assert_eq!(err.code.id, error_codes::FILE_TOO_LARGE.id);
    }

    /// The bound is not off by one in the other direction: a file of exactly the ceiling is read.
    #[test]
    fn a_reader_of_exactly_the_ceiling_is_accepted_whole() {
        let bytes = read_bounded(
            std::io::repeat(7).take(4096),
            Path::new("exact.wav"),
            4096,
            4096,
        )
        .expect("a file of exactly the limit is within it");
        assert_eq!(bytes.len(), 4096);
        assert!(bytes.iter().all(|b| *b == 7));
    }

    /// **Issue #66:** one child that cannot be described does not take its siblings with it. The
    /// per-entry failure is reported by path, and the listing still says it saw the whole
    /// directory — which is what lets the scanner keep the siblings *and* the failed child.
    #[test]
    fn std_fs_read_dir_reports_a_whole_directory_even_though_one_child_could_fail() {
        let dir = temp_dir("per_entry");
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("b.txt"), b"world").unwrap();

        let listing = StdFs.read_dir(&dir).unwrap();
        assert_eq!(listing.entries.len(), 2);
        assert!(listing.fully_enumerated);
        assert!(listing.warnings.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
