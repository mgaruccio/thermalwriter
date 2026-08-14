//! Stable, copyable diagnostics for layout-document authoring.
//!
//! Diagnostics deliberately keep their machine-readable fields separate from
//! their human rendering.  That lets a GUI or another validator serialize the
//! same error without having to parse display text, while the human form can
//! be pasted into a disconnected authoring tool on its own.

use std::{fmt, ops::Range, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Stable code used when TOML cannot be parsed or decoded as a layout document.
pub const TOML_PARSE_CODE: &str = "TWLAYOUT-E001";

/// Stable sample code for an unsupported layout property.
pub const UNSUPPORTED_PROPERTY_CODE: &str = "TWLAYOUT-E014";

/// Severity of a layout diagnostic.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

impl fmt::Display for DiagnosticSeverity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        })
    }
}

/// A stable, self-contained diagnostic emitted while authoring a layout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LayoutDiagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub file: Option<PathBuf>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub profile: Option<String>,
    pub module_id: Option<String>,
    pub property_path: Option<String>,
    pub reason: String,
    pub fix: String,
}

impl LayoutDiagnostic {
    /// Construct a diagnostic without a source location or semantic context.
    pub fn new(
        code: impl Into<String>,
        severity: DiagnosticSeverity,
        message: impl Into<String>,
        reason: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            message: message.into(),
            file: None,
            line: None,
            column: None,
            profile: None,
            module_id: None,
            property_path: None,
            reason: reason.into(),
            fix: fix.into(),
        }
    }

    /// Convert a TOML parser/deserializer error into a stable diagnostic.
    ///
    /// `toml::de::Error::span` reports byte offsets into the original input.
    /// The offsets are converted to one-based line and character-column
    /// coordinates when a span is available.  TOML errors without a span keep
    /// the file, but leave line and column unset.
    pub fn from_toml_error(error: &toml::de::Error, input: &str, file: Option<PathBuf>) -> Self {
        let (line, column) = error
            .span()
            .and_then(|span| line_column_for_span(input, span))
            .map_or((None, None), |(line, column)| (Some(line), Some(column)));
        let reason = error.message().to_owned();

        Self {
            code: TOML_PARSE_CODE.to_owned(),
            severity: DiagnosticSeverity::Error,
            message: "Invalid layout document TOML".to_owned(),
            file,
            line,
            column,
            profile: None,
            module_id: None,
            property_path: None,
            reason,
            fix: "Correct the TOML syntax at the reported location, then validate the layout document again."
                .to_owned(),
        }
    }

    /// Build the stable semantic sample used for unsupported layout properties.
    pub fn unsupported_property(
        file: Option<PathBuf>,
        line: Option<usize>,
        column: Option<usize>,
        profile: Option<String>,
        module_id: Option<String>,
        property_path: Option<String>,
    ) -> Self {
        let property = property_path
            .as_deref()
            .unwrap_or("the selected property")
            .to_owned();

        Self {
            code: UNSUPPORTED_PROPERTY_CODE.to_owned(),
            severity: DiagnosticSeverity::Error,
            message: "Unsupported layout property".to_owned(),
            file,
            line,
            column,
            profile,
            module_id,
            property_path,
            reason: format!(
                "Property `{property}` is not supported by the selected layout module."
            ),
            fix: format!(
                "Remove `{property}` or replace it with a supported property for this module."
            ),
        }
    }

    /// Render this diagnostic as deterministic, standalone human text.
    pub fn to_human(&self) -> String {
        let mut lines = vec![format!(
            "{} [{}] {}",
            self.code, self.severity, self.message
        )];
        lines.push(format!("Location: {}", self.location_text()));

        if let Some(profile) = &self.profile {
            lines.push(format!("Profile: {profile}"));
        }
        if let Some(module_id) = &self.module_id {
            lines.push(format!("Module: {module_id}"));
        }
        if let Some(property_path) = &self.property_path {
            lines.push(format!("Property: {property_path}"));
        }

        lines.push(format!("Reason: {}", self.reason));
        lines.push(format!("Fix: {}", self.fix));
        lines.join("\n")
    }

    /// Serialize this diagnostic as deterministic compact JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("LayoutDiagnostic JSON serialization is infallible")
    }

    /// Serialize this diagnostic as deterministic pretty-printed JSON.
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self)
            .expect("LayoutDiagnostic JSON serialization is infallible")
    }

    /// Deserialize a diagnostic produced by [`Self::to_json`] or
    /// [`Self::to_json_pretty`].
    pub fn from_json(input: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(input)
    }

    fn location_text(&self) -> String {
        match (&self.file, self.line, self.column) {
            (Some(file), Some(line), Some(column)) => {
                format!("{}:{line}:{column}", file.display())
            }
            (Some(file), Some(line), None) => format!("{}:{line}", file.display()),
            (Some(file), None, Some(column)) => format!("{}:?:{column}", file.display()),
            (Some(file), None, None) => file.display().to_string(),
            (None, Some(line), Some(column)) => format!("line {line}, column {column}"),
            (None, Some(line), None) => format!("line {line}"),
            (None, None, Some(column)) => format!("column {column}"),
            (None, None, None) => "document (line/column unavailable)".to_owned(),
        }
    }
}

impl fmt::Display for LayoutDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_human())
    }
}

fn line_column_for_span(input: &str, span: Range<usize>) -> Option<(usize, usize)> {
    let mut offset = span.start.min(input.len());
    while offset > 0 && !input.is_char_boundary(offset) {
        offset -= 1;
    }

    let prefix = &input[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix.rsplit_once('\n').map_or_else(
        || prefix.chars().count() + 1,
        |(_, tail)| tail.chars().count() + 1,
    );
    Some((line, column))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_property_human_output_is_stable() {
        let diagnostic = LayoutDiagnostic::unsupported_property(
            Some(PathBuf::from("layouts/thermal.toml")),
            Some(17),
            Some(9),
            Some("square".to_owned()),
            Some("cpu-temp".to_owned()),
            Some("modules[0].style.glow".to_owned()),
        );

        assert_eq!(
            diagnostic.to_human(),
            "TWLAYOUT-E014 [error] Unsupported layout property\n\
Location: layouts/thermal.toml:17:9\n\
Profile: square\n\
Module: cpu-temp\n\
Property: modules[0].style.glow\n\
Reason: Property `modules[0].style.glow` is not supported by the selected layout module.\n\
Fix: Remove `modules[0].style.glow` or replace it with a supported property for this module."
        );
    }

    #[test]
    fn unsupported_property_json_is_stable_and_round_trips() {
        let diagnostic = LayoutDiagnostic::unsupported_property(
            Some(PathBuf::from("layouts/thermal.toml")),
            Some(17),
            Some(9),
            Some("square".to_owned()),
            Some("cpu-temp".to_owned()),
            Some("modules[0].style.glow".to_owned()),
        );

        assert_eq!(
            diagnostic.to_json(),
            r#"{"code":"TWLAYOUT-E014","severity":"error","message":"Unsupported layout property","file":"layouts/thermal.toml","line":17,"column":9,"profile":"square","module_id":"cpu-temp","property_path":"modules[0].style.glow","reason":"Property `modules[0].style.glow` is not supported by the selected layout module.","fix":"Remove `modules[0].style.glow` or replace it with a supported property for this module."}"#
        );
        assert_eq!(
            LayoutDiagnostic::from_json(&diagnostic.to_json()).expect("diagnostic JSON"),
            diagnostic
        );
    }

    #[test]
    fn toml_error_includes_file_line_and_column_when_span_is_available() {
        let input = "version = 1\nname = \"thermal\n";
        let error = toml::from_str::<toml::Value>(input).expect_err("invalid TOML");
        let diagnostic = LayoutDiagnostic::from_toml_error(
            &error,
            input,
            Some(PathBuf::from("layouts/thermal.toml")),
        );

        assert_eq!(diagnostic.code, TOML_PARSE_CODE);
        assert_eq!(diagnostic.file, Some(PathBuf::from("layouts/thermal.toml")));
        assert_eq!(diagnostic.line, Some(2));
        assert_eq!(diagnostic.column, Some(16));
        assert!(diagnostic.to_human().contains("layouts/thermal.toml:2:16"));
    }
}
