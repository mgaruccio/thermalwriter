// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared path-containment and layout-variable validation used by the daemon
//! and GUI boundaries.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::render::frontmatter::VariableDecl;
pub use crate::render::frontmatter::{contains_template_syntax, is_valid_color};

/// Path traversal / containment failure with enough structure for edge mapping.
#[derive(Debug, Error)]
pub enum PathContainmentError {
    #[error("{kind} name must be relative: {name}")]
    Absolute { kind: &'static str, name: String },

    #[error("{kind} name may not contain '..': {name}")]
    ParentDir { kind: &'static str, name: String },

    #[error("{kind} directory not accessible ({path}): {source}")]
    BaseInaccessible {
        kind: &'static str,
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{kind} not found: {name}")]
    NotFound { kind: &'static str, name: String },

    #[error("{kind} path escapes directory: {name}")]
    Escapes { kind: &'static str, name: String },
}

/// Resolve `name` against `base_dir` and return the canonical path only if it
/// stays within the directory. Rejects absolute paths, `..` components,
/// symlink escapes, and non-existent names. `kind` labels error messages
/// ("Layout", "Background").
pub fn validate_path_within_dir(
    base_dir: &Path,
    name: &str,
    kind: &'static str,
) -> Result<PathBuf, PathContainmentError> {
    let candidate = Path::new(name);
    if candidate.is_absolute() {
        return Err(PathContainmentError::Absolute {
            kind,
            name: name.to_string(),
        });
    }
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(PathContainmentError::ParentDir {
            kind,
            name: name.to_string(),
        });
    }
    let base =
        base_dir
            .canonicalize()
            .map_err(|source| PathContainmentError::BaseInaccessible {
                kind,
                path: base_dir.display().to_string(),
                source,
            })?;
    let resolved = base
        .join(name)
        .canonicalize()
        .map_err(|_| PathContainmentError::NotFound {
            kind,
            name: name.to_string(),
        })?;
    if !resolved.starts_with(&base) {
        return Err(PathContainmentError::Escapes {
            kind,
            name: name.to_string(),
        });
    }
    Ok(resolved)
}

/// Layout-variable validation failure (unknown key, type, range, non-finite).
#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct LayoutVarError(pub String);

/// Validate caller-supplied layout variable overrides against declarations.
pub fn validate_layout_vars(
    declarations: &HashMap<String, VariableDecl>,
    vars: &HashMap<String, String>,
) -> Result<(), LayoutVarError> {
    for (name, value) in vars {
        let Some(decl) = declarations.get(name) else {
            return Err(LayoutVarError(format!("unknown layout variable: {name}")));
        };
        match decl.var_type.as_str() {
            "color" if !is_valid_color(value) => {
                return Err(LayoutVarError(format!(
                    "{name} must be a #rrggbb or #rrggbbaa color"
                )));
            }
            "text" if contains_template_syntax(value) => {
                return Err(LayoutVarError(format!(
                    "{name} may not contain template syntax"
                )));
            }
            "sensor" if value.trim().is_empty() => {
                return Err(LayoutVarError(format!("{name} must select a sensor")));
            }
            "number" => {
                let n = value
                    .parse::<f64>()
                    .map_err(|_| LayoutVarError(format!("{name} must be a number")))?;
                if !n.is_finite() {
                    return Err(LayoutVarError(format!("{name} must be a finite number")));
                }
                if let Some(min) = decl.min
                    && n < min
                {
                    return Err(LayoutVarError(format!("{name} must be ≥ {min}")));
                }
                if let Some(max) = decl.max
                    && n > max
                {
                    return Err(LayoutVarError(format!("{name} must be ≤ {max}")));
                }
            }
            "color" | "text" | "sensor" => {}
            other => {
                return Err(LayoutVarError(format!(
                    "unsupported variable type for {name}: {other}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number_decl(min: Option<f64>, max: Option<f64>) -> HashMap<String, VariableDecl> {
        HashMap::from([(
            "scale".to_string(),
            VariableDecl {
                var_type: "number".to_string(),
                default: "1".to_string(),
                help: String::new(),
                min,
                max,
                step: None,
            },
        )])
    }

    #[test]
    fn rejects_non_finite_numeric_layout_vars() {
        let decls = number_decl(Some(0.0), Some(10.0));
        for bad in ["NaN", "inf", "-inf", "+inf"] {
            let vars = HashMap::from([("scale".to_string(), bad.to_string())]);
            let err = validate_layout_vars(&decls, &vars).expect_err(bad);
            assert!(
                err.0.contains("finite"),
                "expected finite rejection for {bad}, got {err}"
            );
        }
    }

    #[test]
    fn accepts_in_range_finite_number() {
        let decls = number_decl(Some(0.0), Some(10.0));
        let vars = HashMap::from([("scale".to_string(), "3.5".to_string())]);
        assert!(validate_layout_vars(&decls, &vars).is_ok());
    }

    #[test]
    fn path_rejects_parent_dir_and_absolute() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.svg"), "<svg/>").unwrap();
        assert!(matches!(
            validate_path_within_dir(dir.path(), "../x.svg", "Layout"),
            Err(PathContainmentError::ParentDir { .. })
        ));
        assert!(matches!(
            validate_path_within_dir(dir.path(), "/etc/passwd", "Layout"),
            Err(PathContainmentError::Absolute { .. })
        ));
        assert!(validate_path_within_dir(dir.path(), "ok.svg", "Layout").is_ok());
    }
}
