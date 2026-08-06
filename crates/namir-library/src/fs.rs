//! [`ScanFs`]: the filesystem port `scan.rs`'s step machine reads through, with a real
//! [`StdFs`] implementation. Not test hygiene bolted on afterwards — it is what makes FR-LIB-070
//! ("files that disappear, change or are added while Namir is running") a deterministic unit
//! test against a fake instead of a genuine race against the real filesystem, and it is what lets
//! `namir-library/benches/library_scan.rs` (M5, later) measure the incremental-scan logic without
//! materialising a 10,000-file tree for every arm.

use std::path::{Path, PathBuf};

use crate::entry::FileTime;
use crate::error::LibraryError;
use crate::error_codes;

/// One directory entry, as [`ScanFs::read_dir`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryInfo {
    /// The entry's full path.
    pub path: PathBuf,
    /// Whether this entry is a directory (never a symlink to one — see [`StdFs::read_dir`]'s doc
    /// comment on why that's true by construction, not by a check).
    pub is_dir: bool,
    /// Byte length as the directory listing reports it. For a directory, `0` — meaningless and
    /// never consulted.
    pub size: u64,
    /// Last-modified time as the directory listing reports it.
    pub mtime: FileTime,
}

/// The filesystem operations `scan.rs` needs, and nothing else — deliberately narrower than
/// `std::fs` so a fake implementation is easy to write completely and correctly.
pub trait ScanFs: Send + Sync {
    /// Lists `dir`'s immediate children. Does not recurse — the caller (`scan.rs`'s step machine)
    /// owns the traversal order.
    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryInfo>, LibraryError>;

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
    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryInfo>, LibraryError> {
        let entries = std::fs::read_dir(dir).map_err(|e| {
            LibraryError::new(
                error_codes::DIR_UNREADABLE,
                format!("{}: {e}", dir.display()),
            )
        })?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| {
                LibraryError::new(
                    error_codes::DIR_UNREADABLE,
                    format!("{}: {e}", dir.display()),
                )
            })?;
            // DirEntry::file_type() does not follow symlinks on any platform this workspace
            // targets, which is what makes a symlink loop impossible by construction here —
            // this scanner never asks "what does this link point to", only "what is this entry".
            let file_type = entry.file_type().map_err(|e| {
                LibraryError::new(
                    error_codes::DIR_UNREADABLE,
                    format!("{}: {e}", dir.display()),
                )
            })?;
            let metadata = entry.metadata().map_err(|e| {
                LibraryError::new(
                    error_codes::DIR_UNREADABLE,
                    format!("{}: {e}", dir.display()),
                )
            })?;
            out.push(DirEntryInfo {
                path: entry.path(),
                is_dir: file_type.is_dir(),
                size: metadata.len(),
                mtime: FileTime::from_system_time(
                    metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                ),
            });
        }
        Ok(out)
    }

    fn read_file(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, LibraryError> {
        let meta = std::fs::metadata(path).map_err(|e| {
            LibraryError::new(
                error_codes::FILE_UNREADABLE,
                format!("{}: {e}", path.display()),
            )
        })?;
        if meta.len() as usize > max_bytes {
            return Err(LibraryError::new(
                error_codes::FILE_TOO_LARGE,
                format!(
                    "{}: {} bytes, limit {} MB",
                    path.display(),
                    meta.len(),
                    max_bytes / (1024 * 1024)
                ),
            ));
        }
        std::fs::read(path).map_err(|e| {
            LibraryError::new(
                error_codes::FILE_UNREADABLE,
                format!("{}: {e}", path.display()),
            )
        })
    }
}

/// An in-memory [`ScanFs`], `pub(crate)` so `scan.rs`'s own tests can reach it too — a controlled
/// double for cases real disk I/O makes awkward or slow to set up (an oversized file, a
/// millisecond-precise mtime, a directory listing with no filesystem underneath it at all).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeFs {
    dirs: std::collections::HashMap<PathBuf, Vec<DirEntryInfo>>,
    files: std::collections::HashMap<PathBuf, Vec<u8>>,
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
                size,
                mtime,
            });
        self.files.insert(path.clone(), bytes);
        path
    }
}

#[cfg(test)]
impl ScanFs for FakeFs {
    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryInfo>, LibraryError> {
        self.dirs.get(dir).cloned().ok_or_else(|| {
            LibraryError::new(
                error_codes::DIR_UNREADABLE,
                format!("{}: not in fake", dir.display()),
            )
        })
    }

    fn read_file(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, LibraryError> {
        // Mirrors StdFs's own ceiling check against the *claimed* size (the DirEntryInfo added
        // via add_file), not the real byte count -- this is what lets a test simulate an
        // oversized file cheaply, with a small `bytes` payload standing in for content nobody
        // needs to actually read.
        let claimed_size = self
            .dirs
            .values()
            .flatten()
            .find(|e| e.path == path)
            .map(|e| e.size);
        if let Some(size) = claimed_size
            && size as usize > max_bytes
        {
            return Err(LibraryError::new(
                error_codes::FILE_TOO_LARGE,
                format!(
                    "{}: {size} bytes, limit {} MB",
                    path.display(),
                    max_bytes / (1024 * 1024)
                ),
            ));
        }
        self.files.get(path).cloned().ok_or_else(|| {
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

        let entries = StdFs.read_dir(&dir).unwrap();
        assert_eq!(entries.len(), 2);
        let file = entries.iter().find(|e| e.path.ends_with("a.txt")).unwrap();
        assert!(!file.is_dir);
        assert_eq!(file.size, 5);
        let sub = entries.iter().find(|e| e.path.ends_with("sub")).unwrap();
        assert!(sub.is_dir);

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
}
