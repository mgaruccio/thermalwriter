//! Atomic persistence for typed layout documents.
//!
//! Layout documents are complete TOML snapshots.  A save validates and
//! serializes the snapshot before it creates a sibling temporary file, checks
//! the caller's content fingerprint against the current file, and only then
//! replaces the target with an atomic rename.  Legacy SVG/HTML sources are not
//! part of this path.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    diagnostic::{DiagnosticSeverity, LayoutDiagnostic, TOML_PARSE_CODE},
    document::{LayoutDocument, LayoutDocumentError},
};

/// Diagnostic emitted for filesystem or document-persistence failures.
pub const PERSISTENCE_DIAGNOSTIC_CODE: &str = "TWLAYOUT-E040";

/// Diagnostic emitted when the on-disk document no longer matches the
/// fingerprint supplied by the authoring client.
pub const PERSISTENCE_CONFLICT_CODE: &str = "TWLAYOUT-E041";

/// Diagnostic emitted for an unsafe layout name or path.
pub const PERSISTENCE_PATH_CODE: &str = "TWLAYOUT-E042";

/// Diagnostic emitted when a legacy SVG/HTML source is supplied to the typed
/// document persistence path.
pub const LEGACY_LAYOUT_CODE: &str = "TWLAYOUT-E043";

static LAYOUT_WRITE_LOCK: Mutex<()> = Mutex::new(());
static TEMP_SUFFIX: AtomicU64 = AtomicU64::new(0);

/// The result of a successful atomic layout save.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SavedLayout {
    /// The normalized layout name, without the `.layout.toml` suffix.
    pub name: String,
    /// The canonical path that was written.
    pub path: PathBuf,
    /// SHA-256 fingerprint of the exact bytes written to `path`.
    pub fingerprint: String,
}

impl SavedLayout {
    /// Return the content fingerprint under the name used by preview and GUI
    /// boundaries.
    pub fn document_fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Serialize and atomically persist a complete typed layout document.
///
/// `expected_fingerprint` is an optimistic-concurrency guard.  An existing
/// target must be accompanied by its current fingerprint; a missing target
/// must not be created with an expectation for an older file.  This prevents a
/// stale authoring draft from silently replacing an external edit.
pub fn save_layout_document(
    layout_dir: &Path,
    name: &str,
    expected_fingerprint: Option<&str>,
    document: &LayoutDocument,
) -> Result<SavedLayout, LayoutDiagnostic> {
    let _guard = LAYOUT_WRITE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    let safe_name = normalize_layout_name(name).map_err(|error| {
        name_diagnostic(layout_dir, name, error.code(), error.reason(), error.fix())
    })?;

    let root = layout_dir.canonicalize().map_err(|error| {
        io_diagnostic(
            layout_dir,
            "access the layout directory",
            &error,
            "Ensure the configured layout directory exists and is accessible.",
        )
    })?;
    let root_metadata = fs::metadata(&root).map_err(|error| {
        io_diagnostic(
            &root,
            "inspect the layout directory",
            &error,
            "Ensure the configured layout directory is readable.",
        )
    })?;
    if !root_metadata.is_dir() {
        return Err(path_diagnostic(
            &root,
            "The configured layout path is not a directory.",
            "Choose an existing directory for local layout documents.",
        ));
    }

    let target = root.join(format!("{safe_name}.layout.toml"));
    ensure_target_is_contained(&root, &target)?;

    // Serialize and deserialize the complete snapshot before touching the
    // filesystem.  The second pass catches any future serde changes that could
    // produce TOML which is not accepted by the document parser.
    let content = serialize_complete_document(document, &target)?;
    let new_fingerprint = fingerprint(&content);

    let current = current_target(&target)?;
    match (&current, expected_fingerprint) {
        (CurrentTarget::Missing, Some(expected)) => {
            return Err(conflict_diagnostic(
                &target,
                Some(expected),
                None,
                "The expected layout document is no longer present.",
                "Reload the layout directory and save the draft as a new document or retry with a fresh fingerprint.",
            ));
        }
        (
            CurrentTarget::Present {
                fingerprint: actual,
            },
            Some(expected),
        ) if actual != expected => {
            return Err(conflict_diagnostic(
                &target,
                Some(expected),
                Some(actual),
                "The layout document changed on disk after the draft was loaded.",
                "Reload the current document, review the external edit, and save again with its fresh fingerprint.",
            ));
        }
        (
            CurrentTarget::Present {
                fingerprint: actual,
            },
            None,
        ) => {
            return Err(conflict_diagnostic(
                &target,
                None,
                Some(actual),
                "The target layout document already exists and no current fingerprint was supplied.",
                "Load the existing document and provide its fingerprint before replacing it.",
            ));
        }
        (CurrentTarget::Missing, None) | (CurrentTarget::Present { .. }, Some(_)) => {}
    }

    atomic_replace(&target, &content).map_err(|error| {
        io_diagnostic(
            &target,
            "atomically replace the layout document",
            &error,
            "Ensure the layout directory is writable, then retry the save.",
        )
    })?;

    Ok(SavedLayout {
        name: safe_name,
        path: target,
        fingerprint: new_fingerprint,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameErrorCode {
    Invalid,
    Legacy,
}

#[derive(Debug)]
struct NameError {
    code: NameErrorCode,
    reason: String,
    fix: String,
}

impl NameError {
    fn invalid(reason: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            code: NameErrorCode::Invalid,
            reason: reason.into(),
            fix: fix.into(),
        }
    }

    fn legacy(extension: &str) -> Self {
        Self {
            code: NameErrorCode::Legacy,
            reason: format!(
                "legacy {extension} layout sources are not writable as typed documents"
            ),
            fix: "Choose a new composition name without the legacy .svg or .html suffix."
                .to_owned(),
        }
    }

    fn code(&self) -> &'static str {
        match self.code {
            NameErrorCode::Invalid => PERSISTENCE_PATH_CODE,
            NameErrorCode::Legacy => LEGACY_LAYOUT_CODE,
        }
    }

    fn reason(&self) -> &str {
        &self.reason
    }

    fn fix(&self) -> &str {
        &self.fix
    }
}

/// Normalize a user-facing layout name to one safe direct child filename stem.
///
/// Separators and parent components are rejected rather than sanitized.  A
/// save must never turn an attempted traversal into a different, surprising
/// filename.  The typed suffix is accepted idempotently so callers can pass a
/// displayed filename or a bare composition name.
fn normalize_layout_name(name: &str) -> Result<String, NameError> {
    let candidate = name.trim();
    if candidate.is_empty() {
        return Err(NameError::invalid(
            "the layout name is empty",
            "Provide a non-empty composition name.",
        ));
    }
    if candidate.chars().any(char::is_control) {
        return Err(NameError::invalid(
            "the layout name contains a control character",
            "Use letters, numbers, spaces, hyphens, underscores, or dots in the composition name.",
        ));
    }

    let lower = candidate.to_ascii_lowercase();
    let suffix = ".layout.toml";
    let stem = if lower.ends_with(suffix) {
        &candidate[..candidate.len() - suffix.len()]
    } else {
        candidate
    };
    let stem = stem.trim();
    if stem.is_empty() {
        return Err(NameError::invalid(
            "the layout name has no filename stem",
            "Provide a name before the .layout.toml suffix.",
        ));
    }
    if stem.to_ascii_lowercase().ends_with(suffix) {
        return Err(NameError::invalid(
            "the layout name contains the .layout.toml suffix more than once",
            "Provide a bare composition name or one .layout.toml filename.",
        ));
    }

    let stem_lower = stem.to_ascii_lowercase();
    if stem_lower.ends_with(".svg") {
        return Err(NameError::legacy(".svg"));
    }
    if stem_lower.ends_with(".html") || stem_lower.ends_with(".htm") {
        return Err(NameError::legacy(".html"));
    }
    if stem.contains('/') || stem.contains('\\') || stem.contains('\0') {
        return Err(NameError::invalid(
            "the layout name must be one direct filename component",
            "Remove path separators and parent-directory components from the composition name.",
        ));
    }

    let mut components = Path::new(stem).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(NameError::invalid(
            "the layout name is not a safe filename component",
            "Use a relative name without . or .. path components.",
        ));
    }
    if stem.ends_with('.') || stem.ends_with(' ') {
        return Err(NameError::invalid(
            "the layout name may not end with a dot or space",
            "Remove trailing dots or spaces from the composition name.",
        ));
    }

    Ok(stem.to_owned())
}

fn ensure_target_is_contained(root: &Path, target: &Path) -> Result<(), LayoutDiagnostic> {
    let Some(parent) = target.parent() else {
        return Err(path_diagnostic(
            target,
            "The normalized layout target has no parent directory.",
            "Choose a valid configured layout directory.",
        ));
    };
    let canonical_parent = parent.canonicalize().map_err(|error| {
        io_diagnostic(
            parent,
            "resolve the layout target directory",
            &error,
            "Ensure the configured layout directory exists and contains no escaping symlink.",
        )
    })?;
    if !canonical_parent.starts_with(root) {
        return Err(path_diagnostic(
            target,
            "The layout target escapes the configured layout directory.",
            "Choose a direct child name inside the configured layout directory.",
        ));
    }
    if canonical_parent != root {
        return Err(path_diagnostic(
            target,
            "The normalized layout target is not a direct child of the configured layout directory.",
            "Choose a direct child name inside the configured layout directory.",
        ));
    }

    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(path_diagnostic(
                target,
                "The existing layout target is a symlink and cannot be replaced.",
                "Remove the symlink or choose a different composition name; legacy files are left untouched.",
            ));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(path_diagnostic(
                target,
                "The existing layout target is not a regular file.",
                "Choose a different composition name.",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(io_diagnostic(
                target,
                "inspect the layout target",
                &error,
                "Ensure the layout directory is accessible, then retry.",
            ));
        }
    }

    Ok(())
}

enum CurrentTarget {
    Missing,
    Present { fingerprint: String },
}

fn current_target(target: &Path) -> Result<CurrentTarget, LayoutDiagnostic> {
    let metadata = match fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(CurrentTarget::Missing),
        Err(error) => {
            return Err(io_diagnostic(
                target,
                "inspect the current layout document",
                &error,
                "Ensure the current layout document is readable, then retry.",
            ));
        }
    };

    if metadata.file_type().is_symlink() {
        return Err(path_diagnostic(
            target,
            "The existing layout target is a symlink and cannot be replaced.",
            "Remove the symlink or choose a different composition name.",
        ));
    }
    if !metadata.is_file() {
        return Err(path_diagnostic(
            target,
            "The existing layout target is not a regular file.",
            "Choose a different composition name.",
        ));
    }

    let content = fs::read(target).map_err(|error| {
        io_diagnostic(
            target,
            "read the current layout document",
            &error,
            "Ensure the current layout document is readable, then retry.",
        )
    })?;
    Ok(CurrentTarget::Present {
        fingerprint: fingerprint(&content),
    })
}

fn serialize_complete_document(
    document: &LayoutDocument,
    target: &Path,
) -> Result<Vec<u8>, LayoutDiagnostic> {
    let content = document
        .to_toml()
        .map_err(|error| document_diagnostic(target, &error, None))?;
    let parsed = LayoutDocument::from_toml(&content).map_err(|error| {
        let input = Some(content.as_str());
        document_diagnostic(target, &error, input)
    })?;
    if parsed != *document {
        let mut diagnostic = LayoutDiagnostic::new(
            TOML_PARSE_CODE,
            DiagnosticSeverity::Error,
            "Layout document did not round-trip semantically",
            "serializing the complete layout document changed its semantic value",
            "Use only supported layout document fields, then validate the draft again before saving.",
        );
        diagnostic.file = Some(target.to_path_buf());
        return Err(diagnostic);
    }
    Ok(content.into_bytes())
}

fn document_diagnostic(
    target: &Path,
    error: &LayoutDocumentError,
    input: Option<&str>,
) -> LayoutDiagnostic {
    match error {
        LayoutDocumentError::Parse(error) => LayoutDiagnostic::from_toml_error(
            error,
            input.unwrap_or_default(),
            Some(target.to_path_buf()),
        ),
        LayoutDocumentError::Serialize(error) => {
            let mut diagnostic = LayoutDiagnostic::new(
                TOML_PARSE_CODE,
                DiagnosticSeverity::Error,
                "Invalid layout document TOML",
                error.to_string(),
                "Correct the layout document, then validate it again before saving.",
            );
            diagnostic.file = Some(target.to_path_buf());
            diagnostic
        }
        LayoutDocumentError::UnsupportedVersion(version) => {
            let mut diagnostic = LayoutDiagnostic::new(
                TOML_PARSE_CODE,
                DiagnosticSeverity::Error,
                "Unsupported layout document version",
                format!("version {version} is not supported"),
                format!(
                    "Use layout document version {} before saving.",
                    super::document::CURRENT_VERSION
                ),
            );
            diagnostic.file = Some(target.to_path_buf());
            diagnostic
        }
    }
}

fn fingerprint(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn atomic_replace(path: &Path, content: &[u8]) -> io::Result<()> {
    atomic_replace_with_failure(path, content, AtomicFailure::None)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtomicFailure {
    None,
    Write,
    Rename,
}

fn atomic_replace_with_failure(
    path: &Path,
    content: &[u8],
    failure: AtomicFailure,
) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("layout target has no parent: {}", path.display()),
        )
    })?;
    let filename = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("layout target has no filename: {}", path.display()),
        )
    })?;

    for _ in 0..1024 {
        let suffix = TEMP_SUFFIX.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(
            ".{}.tmp.{}.{}",
            filename.to_string_lossy(),
            std::process::id(),
            suffix
        ));
        let mut temp = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };

        let result = (|| {
            if failure == AtomicFailure::Write {
                return Err(io::Error::other("injected layout write failure"));
            }
            temp.write_all(content)?;
            temp.sync_all()?;
            drop(temp);
            if failure == AtomicFailure::Rename {
                return Err(io::Error::other("injected layout rename failure"));
            }
            fs::rename(&temp_path, path)
        })();

        // `temp` is dropped by the closure on every error path.  Removing the
        // sibling after the handle is closed keeps failed saves from leaving
        // stale drafts behind (where the filesystem permits cleanup).
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        return result;
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate a unique sibling temporary file",
    ))
}

fn name_diagnostic(
    layout_dir: &Path,
    name: &str,
    code: &str,
    reason: &str,
    fix: &str,
) -> LayoutDiagnostic {
    let mut diagnostic = LayoutDiagnostic::new(
        code,
        DiagnosticSeverity::Error,
        "Invalid layout document name",
        format!("{reason}: `{name}`"),
        fix,
    );
    diagnostic.file = Some(layout_dir.to_path_buf());
    diagnostic
}

fn path_diagnostic(
    path: &Path,
    reason: impl Into<String>,
    fix: impl Into<String>,
) -> LayoutDiagnostic {
    let mut diagnostic = LayoutDiagnostic::new(
        PERSISTENCE_PATH_CODE,
        DiagnosticSeverity::Error,
        "Unsafe layout document path",
        reason,
        fix,
    );
    diagnostic.file = Some(path.to_path_buf());
    diagnostic
}

fn io_diagnostic(path: &Path, operation: &str, error: &io::Error, fix: &str) -> LayoutDiagnostic {
    let mut diagnostic = LayoutDiagnostic::new(
        PERSISTENCE_DIAGNOSTIC_CODE,
        DiagnosticSeverity::Error,
        "Unable to save layout document",
        format!("failed to {operation} `{}`: {error}", path.display()),
        fix,
    );
    diagnostic.file = Some(path.to_path_buf());
    diagnostic
}

fn conflict_diagnostic(
    path: &Path,
    expected: Option<&str>,
    actual: Option<&str>,
    reason: &str,
    fix: &str,
) -> LayoutDiagnostic {
    let expected = expected.unwrap_or("<none>");
    let actual = actual.unwrap_or("<missing>");
    let mut diagnostic = LayoutDiagnostic::new(
        PERSISTENCE_CONFLICT_CODE,
        DiagnosticSeverity::Error,
        "Layout document save conflict",
        format!("{reason} expected fingerprint `{expected}`, current fingerprint `{actual}`"),
        fix,
    );
    diagnostic.file = Some(path.to_path_buf());
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs};

    use tempfile::TempDir;

    fn document(name: &str) -> LayoutDocument {
        LayoutDocument {
            version: super::super::document::CURRENT_VERSION,
            name: name.to_owned(),
            preset: Some("test".to_owned()),
            modules: Vec::new(),
            profiles: BTreeMap::new(),
        }
    }

    fn temp_files(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .expect("read layout directory")
            .map(|entry| entry.expect("directory entry").path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(".tmp.")
            })
            .collect()
    }

    #[test]
    fn new_document_is_written_and_round_trips_semantically() {
        let temp = TempDir::new().expect("temp layout directory");
        let draft = document("draft");

        let saved = save_layout_document(temp.path(), "draft", None, &draft).expect("save");

        assert_eq!(saved.name, "draft");
        assert_eq!(saved.path, temp.path().join("draft.layout.toml"));
        assert_eq!(
            saved.fingerprint,
            fingerprint(&fs::read(&saved.path).unwrap())
        );
        let reloaded = LayoutDocument::from_toml(&fs::read_to_string(&saved.path).unwrap())
            .expect("reload saved TOML");
        assert_eq!(reloaded, draft);
        assert!(temp_files(temp.path()).is_empty());
    }

    #[test]
    fn filename_suffix_is_normalized_without_rewriting_legacy_sources() {
        let temp = TempDir::new().expect("temp layout directory");
        let legacy = temp.path().join("legacy.svg");
        fs::write(&legacy, "<svg>legacy</svg>").expect("legacy source");

        let saved =
            save_layout_document(temp.path(), "draft.layout.toml", None, &document("draft"))
                .expect("save normalized name");
        assert_eq!(saved.path, temp.path().join("draft.layout.toml"));
        assert_eq!(fs::read_to_string(legacy).unwrap(), "<svg>legacy</svg>");

        let error = save_layout_document(temp.path(), "legacy.svg", None, &document("draft"))
            .expect_err("legacy source name must be rejected");
        assert_eq!(error.code, LEGACY_LAYOUT_CODE);
        assert_eq!(
            fs::read_to_string(temp.path().join("legacy.svg")).unwrap(),
            "<svg>legacy</svg>"
        );
    }

    #[test]
    fn mismatched_fingerprint_preserves_current_file_and_draft() {
        let temp = TempDir::new().expect("temp layout directory");
        let original = document("original");
        let saved =
            save_layout_document(temp.path(), "shared", None, &original).expect("initial save");
        let current = "externally edited\n";
        fs::write(&saved.path, current).expect("external edit");
        let draft = document("draft");

        let error = save_layout_document(temp.path(), "shared", Some(&saved.fingerprint), &draft)
            .expect_err("stale save must conflict");

        assert_eq!(error.code, PERSISTENCE_CONFLICT_CODE);
        assert_eq!(fs::read_to_string(&saved.path).unwrap(), current);
        assert_eq!(draft.name, "draft");
        assert!(temp_files(temp.path()).is_empty());
    }

    #[test]
    fn invalid_document_and_path_write_nothing() {
        let temp = TempDir::new().expect("temp layout directory");
        let mut invalid = document("invalid");
        invalid.version += 1;

        let error = save_layout_document(temp.path(), "invalid", None, &invalid)
            .expect_err("unsupported version must be rejected");
        assert_eq!(error.code, TOML_PARSE_CODE);
        assert!(fs::read_dir(temp.path()).unwrap().next().is_none());

        let error = save_layout_document(temp.path(), "../escape", None, &document("draft"))
            .expect_err("traversal must be rejected");
        assert_eq!(error.code, PERSISTENCE_PATH_CODE);
        assert!(fs::read_dir(temp.path()).unwrap().next().is_none());
        assert!(
            !temp
                .path()
                .parent()
                .unwrap()
                .join("escape.layout.toml")
                .exists()
        );
    }

    #[test]
    fn symlink_target_is_rejected_without_touching_outside_file() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let temp = TempDir::new().expect("temp layout directory");
            let outside = temp.path().parent().unwrap().join("outside-layout.toml");
            fs::write(&outside, "outside").expect("outside source");
            symlink(&outside, temp.path().join("escape.layout.toml")).expect("symlink");

            let error = save_layout_document(temp.path(), "escape", None, &document("draft"))
                .expect_err("escaping symlink must be rejected");
            assert_eq!(error.code, PERSISTENCE_PATH_CODE);
            assert_eq!(fs::read_to_string(outside).unwrap(), "outside");
            assert!(temp_files(temp.path()).is_empty());
        }
    }

    #[test]
    fn injected_write_and_rename_failures_preserve_original_and_cleanup_temp() {
        let temp = TempDir::new().expect("temp layout directory");
        let target = temp.path().join("shared.layout.toml");
        let original = b"original";
        fs::write(&target, original).expect("original file");

        let error = atomic_replace_with_failure(&target, b"new", AtomicFailure::Write)
            .expect_err("injected write failure");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&target).unwrap(), original);
        assert!(temp_files(temp.path()).is_empty());

        let error = atomic_replace_with_failure(&target, b"new", AtomicFailure::Rename)
            .expect_err("injected rename failure");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&target).unwrap(), original);
        assert!(temp_files(temp.path()).is_empty());
    }
}
