//! D-11.3: a file reference is a record of all three of library-relative path, absolute path,
//! and BLAKE3 content hash — plus, per FR-STATE-080, an optional embedded copy of the resource
//! itself. This module owns the *shape*; resolving one against a real filesystem or library is
//! `resolve.rs`'s job (and, ultimately, `namir-worker`'s — see that module's doc comment).

use std::path::{Component, Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use namir_core::ContentHash;
use serde_json::{Map, Value};

use crate::error::StateError;
use crate::error_codes;

/// A library-relative path as stored in a document: `/`-separated, no `.`/`..` segments, no
/// leading separator, no drive prefix, non-empty. This is where NFR-PORT-050's "path separators
/// shall be handled such that files written on one platform load identically on another" is
/// actually closed — once, in the type, rather than by convention at every call site that
/// touches a stored reference.
///
/// **Decision:** accept either separator on input, normalise to `/` on the way in, and never
/// re-derive a platform separator from the stored string — [`Self::join_onto`] appends segments
/// onto a caller-supplied root via `PathBuf::push`, which uses the *host* platform's own
/// separator regardless of what character the stored string used. So a Windows-authored
/// `"cabs/1960a.wav"` and (hypothetically) a Windows-authored `"cabs\\1960a.wav"` both parse to
/// the identical `RelPath`, and both resolve correctly on Linux, because the stored form is
/// never platform syntax at all — it is a sequence of segments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelPath(String);

/// Why [`RelPath::parse`]/[`RelPath::from_relative_path`] rejected a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelPathError {
    /// The path had no segments at all.
    Empty,
    /// The path was rooted (a leading `/`) or carried a Windows drive prefix (`C:`) — a
    /// library-relative path is relative by definition; an absolute one belongs in
    /// [`FileRef::absolute`] instead.
    NotRelative,
    /// A `..` segment — a library-relative path can never point outside the root it is relative
    /// to; nothing in this format needs that and P8 ("failure degrades; it does not propagate")
    /// argues against ever giving a stored path the power to escape a configured directory.
    ParentTraversal,
    /// An empty segment (`a//b`) or a bare `.` segment (`a/./b`) — both are ambiguous rather than
    /// meaningful, and rejecting them outright is simpler and safer than defining a normalisation
    /// rule for them.
    DegenerateSegment,
}

impl std::fmt::Display for RelPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "path has no segments"),
            Self::NotRelative => write!(f, "path must be relative, not absolute or drive-rooted"),
            Self::ParentTraversal => write!(f, "path must not contain a \"..\" segment"),
            Self::DegenerateSegment => write!(f, "path must not contain an empty or \".\" segment"),
        }
    }
}

impl std::error::Error for RelPathError {}

impl RelPath {
    /// Parses a stored (or hand-edited, per FR-STATE-040) library-relative path string. Accepts
    /// either separator on input; see this type's doc comment for why that is safe.
    pub fn parse(text: &str) -> Result<RelPath, RelPathError> {
        if text.is_empty() {
            return Err(RelPathError::Empty);
        }
        let normalised = text.replace('\\', "/");
        if normalised.starts_with('/') {
            return Err(RelPathError::NotRelative);
        }
        // A Windows drive prefix ("C:...") — checked textually since this string is never
        // handed to std::path on the parsing side (that would reintroduce the platform-syntax
        // dependency this type exists to avoid).
        if normalised.len() >= 2 && normalised.as_bytes().get(1) == Some(&b':') {
            return Err(RelPathError::NotRelative);
        }
        let mut segments = Vec::new();
        for segment in normalised.split('/') {
            match segment {
                "" | "." => return Err(RelPathError::DegenerateSegment),
                ".." => return Err(RelPathError::ParentTraversal),
                s => segments.push(s),
            }
        }
        Ok(RelPath(segments.join("/")))
    }

    /// Builds a `RelPath` from a real [`Path`] already known to be relative (e.g. the result of
    /// [`Path::strip_prefix`]ing a library root off a scanned file's path) — the one place this
    /// type consults `std::path` at all, and it does so component-by-component rather than by
    /// string surgery, so it is correct regardless of the host platform's own separator.
    pub fn from_relative_path(path: &Path) -> Result<RelPath, RelPathError> {
        let mut segments = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(s) => segments.push(s.to_string_lossy().into_owned()),
                Component::ParentDir => return Err(RelPathError::ParentTraversal),
                Component::CurDir => return Err(RelPathError::DegenerateSegment),
                Component::RootDir | Component::Prefix(_) => {
                    return Err(RelPathError::NotRelative);
                }
            }
        }
        if segments.is_empty() {
            return Err(RelPathError::Empty);
        }
        Ok(RelPath(segments.join("/")))
    }

    /// The stored form: `/`-separated segments, exactly as written into a document.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Joins this path's segments onto `root` using the **host platform's own** path-joining
    /// (`PathBuf::push`), never the stored `/` characters directly — this is what makes a
    /// Windows-authored reference resolve correctly on Linux and vice versa.
    pub fn join_onto(&self, root: &Path) -> PathBuf {
        let mut path = root.to_path_buf();
        for segment in self.0.split('/') {
            path.push(segment);
        }
        path
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// NFR-SEC-020's bound on an [`EmbeddedRef`]'s decoded byte size. In practice this is already
/// implied by [`crate::MAX_DOCUMENT_BYTES`] — the base64 text an embedded resource's bytes are
/// stored as is itself part of the same document, so it can never exceed roughly
/// `MAX_DOCUMENT_BYTES` to begin with, and base64 decoding allocates in proportion to the input
/// actually present (not a separately-declared length field the way a WAV header's `data` chunk
/// length is — there is no forgeable "claims more than it delivers" vector here the way
/// `namir_ir::wav`'s module doc warns about for WAV). This constant is kept anyway, set to
/// exactly [`crate::MAX_DOCUMENT_BYTES`], as the single place that bound is *stated* for this
/// specific case rather than left to be re-derived from a different module's constant — the
/// NFR's own wording asks for a *documented* bound, not merely an accidentally-true one.
pub const MAX_EMBEDDED_BYTES: usize = crate::document::MAX_DOCUMENT_BYTES;

/// FR-STATE-080's optional embedded copy of a model or IR's raw bytes, carried directly in the
/// state document. A `format_version` bump was the alternative to reserving this from the start
/// — see `docs/02-architecture.md`'s M5 note on D-11.1 — so the shape exists from this crate's
/// first version rather than being retrofitted.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedRef {
    /// The embedded resource's media type (`"application/vnd.namir.nam+json"` for a model,
    /// `"audio/wav"` for an IR) — informational, not consulted by this crate; a caller decides
    /// what to do with the bytes based on which of [`FileRef`]'s two slots (`nam`/`ir` on
    /// [`crate::State`]) carried this reference, not by parsing this field.
    pub media_type: String,
    /// The resource's raw, decoded bytes.
    pub data: Vec<u8>,
}

impl EmbeddedRef {
    fn to_value(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("encoding".to_string(), Value::from("base64"));
        obj.insert(
            "media_type".to_string(),
            Value::from(self.media_type.clone()),
        );
        obj.insert("data".to_string(), Value::from(BASE64.encode(&self.data)));
        Value::Object(obj)
    }

    fn from_value(value: &Value) -> Result<EmbeddedRef, StateError> {
        let obj = value.as_object().ok_or_else(|| {
            StateError::new(error_codes::MALFORMED_JSON, "embedded must be an object")
        })?;
        let encoding = obj.get("encoding").and_then(Value::as_str).ok_or_else(|| {
            StateError::new(
                error_codes::MALFORMED_JSON,
                "embedded.encoding must be a string",
            )
        })?;
        if encoding != "base64" {
            return Err(StateError::new(
                error_codes::MALFORMED_JSON,
                format!("embedded.encoding \"{encoding}\" is not supported (only \"base64\")"),
            ));
        }
        let media_type = obj
            .get("media_type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let data_text = obj.get("data").and_then(Value::as_str).ok_or_else(|| {
            StateError::new(
                error_codes::MALFORMED_JSON,
                "embedded.data must be a string",
            )
        })?;
        // NFR-SEC-020: reject on the *encoded* length before decoding -- see MAX_EMBEDDED_BYTES's
        // doc comment for why this is already implied by the whole-document ceiling, and kept
        // explicit anyway.
        if data_text.len() > MAX_EMBEDDED_BYTES {
            return Err(StateError::new(
                error_codes::DOCUMENT_TOO_LARGE,
                format!(
                    "embedded resource is {} bytes encoded, limit {} MB",
                    data_text.len(),
                    MAX_EMBEDDED_BYTES / (1024 * 1024)
                ),
            ));
        }
        let data = BASE64.decode(data_text).map_err(|e| {
            StateError::new(error_codes::MALFORMED_JSON, format!("embedded.data: {e}"))
        })?;
        Ok(EmbeddedRef { media_type, data })
    }
}

/// D-11.3: all three of library-relative path, absolute path and content hash, plus FR-STATE-080's
/// optional embedded copy. `hash` is the identity (P7); the two paths are hints, and `embedded`,
/// when present, is the resource itself rather than a pointer to one.
///
/// **Known limitation, recorded rather than silently accepted:** unlike [`crate::Document`],
/// this type has no carrier for a field it doesn't recognise — `to_value`/`from_value` only ever
/// round-trip the five fields declared below. If a future version of this format adds a sixth
/// field to a `FileRef` (an `origin` hint, a Tone3000 id — RD-1 territory), an older build's
/// load-modify-save cycle will silently drop it, unlike an unrecognised top-level section or an
/// unrecognised `parameters` key, both of which D-11.2 genuinely preserves today. Accepted for M5
/// because no such field exists yet to lose; the fix, when one is added, is giving this type its
/// own `extra: Map<String, Value>` carrier, following `Document`'s own pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct FileRef {
    /// P7: "identity of a model or IR is its content hash." Never optional — every `FileRef`
    /// has one, which is what makes `resolve::candidates`' third step always exist.
    pub hash: ContentHash,
    /// D-11.3's first resolution step. Relative to **whichever configured library root a
    /// resolver's `resolve_library_relative` finds it under** — a `FileRef` deliberately stores
    /// no root identity of its own (see `resolve.rs`'s module doc comment for why: recording one
    /// would embed machine-specific data in the one field meant to survive UC-3, sending a
    /// project to someone whose library roots are configured differently).
    pub library_relative: Option<RelPath>,
    /// D-11.3's second resolution step: the originating platform's absolute path, verbatim and
    /// **opaque**. Never parsed structurally by this crate — parsing it would mean parsing a
    /// foreign platform's path syntax, which is exactly what NFR-PORT-050 forbids assuming.
    pub absolute: Option<String>,
    /// FR-STATE-070's "the user shall be shown the missing file's name" — stored explicitly
    /// rather than derived from `absolute`, because deriving it would require splitting a
    /// possibly-foreign-platform path string, the same dependency `absolute` itself avoids.
    pub display_name: String,
    /// FR-STATE-080's reservation, exercised: when present, this build both reads and writes an
    /// embedded copy of the resource (M5's Should-scope decision — see the milestone record).
    pub embedded: Option<EmbeddedRef>,
}

impl FileRef {
    pub(crate) fn to_value(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("hash".to_string(), Value::from(self.hash.to_string()));
        if let Some(rel) = &self.library_relative {
            obj.insert("library_relative".to_string(), Value::from(rel.as_str()));
        }
        if let Some(abs) = &self.absolute {
            obj.insert("absolute".to_string(), Value::from(abs.clone()));
        }
        obj.insert(
            "display_name".to_string(),
            Value::from(self.display_name.clone()),
        );
        if let Some(embedded) = &self.embedded {
            obj.insert("embedded".to_string(), embedded.to_value());
        }
        Value::Object(obj)
    }

    pub(crate) fn from_value(value: &Value) -> Result<FileRef, StateError> {
        let obj = value.as_object().ok_or_else(|| {
            StateError::new(
                error_codes::MALFORMED_JSON,
                "file reference must be an object",
            )
        })?;
        let hash_text = obj.get("hash").and_then(Value::as_str).ok_or_else(|| {
            StateError::new(
                error_codes::MALFORMED_JSON,
                "file reference is missing \"hash\"",
            )
        })?;
        let hash = hash_text
            .parse::<ContentHash>()
            .map_err(|e| StateError::new(error_codes::MALFORMED_JSON, format!("hash: {e}")))?;
        // A malformed library_relative string (foreign traversal, absolute) is tolerated rather
        // than rejecting the whole document -- D-11.2's tolerant-loading intent applied to a
        // single field: the reference simply loses that one resolution candidate, matching how
        // an unrecognised parameter loses only itself, not the whole document.
        let library_relative = obj
            .get("library_relative")
            .and_then(Value::as_str)
            .and_then(|s| RelPath::parse(s).ok());
        let absolute = obj
            .get("absolute")
            .and_then(Value::as_str)
            .map(str::to_string);
        let display_name = obj
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let embedded = match obj.get("embedded") {
            Some(v) => Some(EmbeddedRef::from_value(v)?),
            None => None,
        };
        Ok(FileRef {
            hash,
            library_relative,
            absolute,
            display_name,
            embedded,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------------------
    // RelPath
    // -----------------------------------------------------------------------------------

    #[test]
    fn parses_forward_slash_separated_paths() {
        let p = RelPath::parse("cabs/1960a.wav").unwrap();
        assert_eq!(p.as_str(), "cabs/1960a.wav");
    }

    /// NFR-PORT-050's core content: a backslash-separated (Windows-authored) path normalises to
    /// the same stored form as a forward-slash one.
    #[test]
    fn normalises_backslashes_to_forward_slashes() {
        let from_windows = RelPath::parse("cabs\\1960a.wav").unwrap();
        let from_posix = RelPath::parse("cabs/1960a.wav").unwrap();
        assert_eq!(from_windows, from_posix);
    }

    #[test]
    fn rejects_empty_path() {
        assert_eq!(RelPath::parse(""), Err(RelPathError::Empty));
    }

    #[test]
    fn rejects_a_leading_slash() {
        assert_eq!(
            RelPath::parse("/cabs/1960a.wav"),
            Err(RelPathError::NotRelative)
        );
    }

    #[test]
    fn rejects_a_windows_drive_prefix() {
        assert_eq!(
            RelPath::parse("C:\\Users\\erwan\\cabs\\1960a.wav"),
            Err(RelPathError::NotRelative)
        );
    }

    #[test]
    fn rejects_parent_traversal() {
        assert_eq!(
            RelPath::parse("../../etc/passwd"),
            Err(RelPathError::ParentTraversal)
        );
        assert_eq!(
            RelPath::parse("cabs/../1960a.wav"),
            Err(RelPathError::ParentTraversal)
        );
    }

    #[test]
    fn rejects_degenerate_segments() {
        assert_eq!(
            RelPath::parse("cabs//1960a.wav"),
            Err(RelPathError::DegenerateSegment)
        );
        assert_eq!(
            RelPath::parse("cabs/./1960a.wav"),
            Err(RelPathError::DegenerateSegment)
        );
    }

    #[test]
    fn from_relative_path_matches_parse_for_a_normal_path() {
        let from_path = RelPath::from_relative_path(Path::new("cabs/1960a.wav")).unwrap();
        let from_str = RelPath::parse("cabs/1960a.wav").unwrap();
        assert_eq!(from_path, from_str);
    }

    #[test]
    fn from_relative_path_rejects_an_absolute_path() {
        let err = RelPath::from_relative_path(Path::new("/etc/passwd")).unwrap_err();
        assert_eq!(err, RelPathError::NotRelative);
    }

    #[test]
    fn join_onto_uses_the_host_platform_separator() {
        let p = RelPath::parse("cabs/1960a.wav").unwrap();
        let joined = p.join_onto(Path::new("/library"));
        assert_eq!(joined, Path::new("/library").join("cabs").join("1960a.wav"));
    }

    // -----------------------------------------------------------------------------------
    // FileRef / EmbeddedRef round trip
    // -----------------------------------------------------------------------------------

    fn sample_ref() -> FileRef {
        FileRef {
            hash: ContentHash::of(b"a model file"),
            library_relative: Some(RelPath::parse("marshall/plexi.nam").unwrap()),
            absolute: Some("C:\\Users\\erwan\\Models\\marshall\\plexi.nam".to_string()),
            display_name: "plexi.nam".to_string(),
            embedded: None,
        }
    }

    #[test]
    fn file_ref_round_trips_through_value() {
        let original = sample_ref();
        let value = original.to_value();
        let restored = FileRef::from_value(&value).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn file_ref_with_embedded_data_round_trips() {
        let mut original = sample_ref();
        original.embedded = Some(EmbeddedRef {
            media_type: "application/vnd.namir.nam+json".to_string(),
            data: b"{\"fake\": \"nam json\"}".to_vec(),
        });
        let value = original.to_value();
        let restored = FileRef::from_value(&value).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn file_ref_without_optional_fields_round_trips() {
        let original = FileRef {
            hash: ContentHash::of(b"minimal"),
            library_relative: None,
            absolute: None,
            display_name: "minimal.wav".to_string(),
            embedded: None,
        };
        let restored = FileRef::from_value(&original.to_value()).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn from_value_rejects_a_missing_hash() {
        let mut obj = Map::new();
        obj.insert("display_name".to_string(), Value::from("x.nam"));
        let err = FileRef::from_value(&Value::Object(obj)).unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_JSON.id);
    }

    #[test]
    fn from_value_rejects_an_unparseable_hash() {
        let mut obj = Map::new();
        obj.insert("hash".to_string(), Value::from("not a hex hash"));
        obj.insert("display_name".to_string(), Value::from("x.nam"));
        let err = FileRef::from_value(&Value::Object(obj)).unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_JSON.id);
    }

    /// D-11.2's tolerant intent applied to a single field: a malformed `library_relative` (here,
    /// one carrying `..`) doesn't fail the whole reference, it just isn't offered as a candidate.
    #[test]
    fn from_value_tolerates_a_malformed_library_relative_by_dropping_it() {
        let mut obj = Map::new();
        obj.insert(
            "hash".to_string(),
            Value::from(ContentHash::of(b"x").to_string()),
        );
        obj.insert("library_relative".to_string(), Value::from("../escape.nam"));
        obj.insert("display_name".to_string(), Value::from("x.nam"));
        let restored = FileRef::from_value(&Value::Object(obj)).unwrap();
        assert_eq!(restored.library_relative, None);
    }

    #[test]
    fn embedded_ref_rejects_an_unsupported_encoding() {
        let mut obj = Map::new();
        obj.insert("encoding".to_string(), Value::from("hex"));
        obj.insert("data".to_string(), Value::from("deadbeef"));
        let err = EmbeddedRef::from_value(&Value::Object(obj)).unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_JSON.id);
    }

    #[test]
    fn embedded_ref_rejects_invalid_base64() {
        let mut obj = Map::new();
        obj.insert("encoding".to_string(), Value::from("base64"));
        obj.insert("data".to_string(), Value::from("not valid base64!!"));
        let err = EmbeddedRef::from_value(&Value::Object(obj)).unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_JSON.id);
    }

    /// NFR-SEC-020: an embedded blob whose *encoded* text exceeds the ceiling is rejected before
    /// `base64::decode` is ever called on it, so decoding never allocates from an unbounded
    /// input. Uses a byte count just over the limit, generated cheaply (a repeated ASCII
    /// character is trivial to allocate at this size and is valid base64 alphabet input).
    #[test]
    fn embedded_ref_rejects_encoded_data_over_the_ceiling_before_decoding() {
        let oversized_text = "A".repeat(MAX_EMBEDDED_BYTES + 4);
        let mut obj = Map::new();
        obj.insert("encoding".to_string(), Value::from("base64"));
        obj.insert("data".to_string(), Value::from(oversized_text));
        let err = EmbeddedRef::from_value(&Value::Object(obj)).unwrap_err();
        assert_eq!(err.code.id, error_codes::DOCUMENT_TOO_LARGE.id);
    }
}
