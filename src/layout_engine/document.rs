//! Versioned, bounded layout documents shared by the daemon and GUI.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::scene::ImageFit;

/// The only layout document version understood by this crate.
pub const CURRENT_VERSION: u32 = 1;

/// A persisted, preset-oriented layout composition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LayoutDocument {
    pub version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    pub modules: Vec<ModuleDocument>,
    pub profiles: BTreeMap<String, ProfileRecipeDocument>,
}

/// A typed module in document order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ModuleDocument {
    Metric(MetricDocument),
    Sparkline(SparklineDocument),
    Text(TextDocument),
    Media(MediaDocument),
}

/// A numeric or status metric module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MetricDocument {
    pub id: String,
    pub binding: String,
    pub variant: String,
}

/// A time-series sparkline module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SparklineDocument {
    pub id: String,
    pub binding: String,
    pub variant: String,
}

/// A text module bound to a catalog value or text source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TextDocument {
    pub id: String,
    pub binding: String,
    pub variant: String,
}

/// A media module bound to a catalog media source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct MediaDocument {
    pub id: String,
    pub binding: String,
    pub variant: String,
    /// Optional local media path. An empty path falls back to `binding`.
    #[serde(default, skip_serializing_if = "is_empty_path")]
    pub source: PathBuf,
    /// How the decoded image fills the solved bounds.
    #[serde(default, skip_serializing_if = "is_default_media_fit")]
    pub fit: ImageFit,
    /// Request bridge spanning when the selected profile policy permits it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub span_bridge: bool,
    /// Image opacity, bounded by the media emitter before scene emission.
    #[serde(
        default = "default_media_opacity",
        skip_serializing_if = "is_default_media_opacity"
    )]
    pub opacity: f32,
}

fn default_media_opacity() -> f32 {
    1.0
}

fn is_empty_path(value: &Path) -> bool {
    value.as_os_str().is_empty()
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_default_media_opacity(value: &f32) -> bool {
    *value == default_media_opacity()
}

fn is_default_media_fit(value: &ImageFit) -> bool {
    *value == ImageFit::Contain
}

/// A named, bounded composition recipe for a target profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProfileRecipeDocument {
    pub recipe: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<String>,
}

/// Errors returned while parsing or serializing a layout document.
#[derive(Debug, Error)]
pub enum LayoutDocumentError {
    #[error("failed to parse layout document TOML: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("failed to serialize layout document TOML: {0}")]
    Serialize(#[from] toml::ser::Error),

    #[error("unsupported layout document version {0}; expected version {CURRENT_VERSION}")]
    UnsupportedVersion(u32),
}

impl LayoutDocument {
    /// Parse and version-check a layout document from TOML.
    pub fn from_toml(input: &str) -> Result<Self, LayoutDocumentError> {
        let document: Self = toml::from_str(input)?;
        document.validate_version()?;
        Ok(document)
    }

    /// Alias for [`LayoutDocument::from_toml`] for callers that prefer an
    /// explicitly named parser.
    pub fn parse_toml(input: &str) -> Result<Self, LayoutDocumentError> {
        Self::from_toml(input)
    }

    /// Serialize a supported layout document as canonical pretty TOML.
    pub fn to_toml(&self) -> Result<String, LayoutDocumentError> {
        self.validate_version()?;
        Ok(toml::to_string_pretty(self)?)
    }

    /// Reject versions that this implementation cannot interpret.
    pub fn validate_version(&self) -> Result<(), LayoutDocumentError> {
        if self.version == CURRENT_VERSION {
            Ok(())
        } else {
            Err(LayoutDocumentError::UnsupportedVersion(self.version))
        }
    }
}

impl FromStr for LayoutDocument {
    type Err = LayoutDocumentError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::from_toml(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL_TOML: &str = r#"
version = 1
name = "thermal-overview"
preset = "neon-composer"

[[modules]]
id = "cpu-temp"
kind = "metric"
binding = "cpu.temperature"
variant = "hero"

[[modules]]
id = "history"
kind = "sparkline"
binding = "cpu.temperature.history"
variant = "neon"

[profiles.square]
recipe = "column"

[profiles.wide]
recipe = "two-column"

[profiles.thermalright-curved-2400x1080]
recipe = "zoned-panorama"
bridge = "media-only"
"#;

    fn module_document(kind: &str) -> String {
        format!(
            "version = 1\nname = \"module-test\"\nmodules = [{{ id = \"module\", kind = \"{kind}\", binding = \"sensor.value\", variant = \"default\" }}]\nprofiles = {{}}\n"
        )
    }

    #[test]
    fn parses_canonical_document_and_profile_table() {
        let document = LayoutDocument::from_toml(CANONICAL_TOML).expect("canonical TOML");

        assert_eq!(document.version, CURRENT_VERSION);
        assert_eq!(document.name, "thermal-overview");
        assert_eq!(document.preset.as_deref(), Some("neon-composer"));
        assert_eq!(document.modules.len(), 2);
        assert!(matches!(
            &document.modules[0],
            ModuleDocument::Metric(MetricDocument {
                id,
                binding,
                variant
            }) if id == "cpu-temp" && binding == "cpu.temperature" && variant == "hero"
        ));
        assert!(matches!(
            &document.modules[1],
            ModuleDocument::Sparkline(SparklineDocument {
                id,
                binding,
                variant
            }) if id == "history" && binding == "cpu.temperature.history" && variant == "neon"
        ));

        assert_eq!(document.profiles.len(), 3);
        assert_eq!(document.profiles["square"].recipe, "column");
        assert_eq!(document.profiles["square"].bridge, None);
        assert_eq!(document.profiles["wide"].recipe, "two-column");
        assert_eq!(document.profiles["wide"].bridge, None);
        assert_eq!(
            document.profiles["thermalright-curved-2400x1080"].recipe,
            "zoned-panorama"
        );
        assert_eq!(
            document.profiles["thermalright-curved-2400x1080"]
                .bridge
                .as_deref(),
            Some("media-only")
        );
    }

    #[test]
    fn serializes_and_round_trips_without_reordering_modules() {
        let document = LayoutDocument::from_toml(CANONICAL_TOML).expect("canonical TOML");
        let serialized = document.to_toml().expect("serialize TOML");
        let round_tripped = LayoutDocument::from_toml(&serialized).expect("round trip TOML");

        assert_eq!(round_tripped, document);
        let module_ids: Vec<&str> = round_tripped
            .modules
            .iter()
            .map(|module| match module {
                ModuleDocument::Metric(module) => module.id.as_str(),
                ModuleDocument::Sparkline(module) => module.id.as_str(),
                ModuleDocument::Text(module) => module.id.as_str(),
                ModuleDocument::Media(module) => module.id.as_str(),
            })
            .collect();
        assert_eq!(module_ids, ["cpu-temp", "history"]);
    }

    #[test]
    fn parses_and_serializes_every_module_variant() {
        for (kind, expected) in [
            ("metric", "metric"),
            ("sparkline", "sparkline"),
            ("text", "text"),
            ("media", "media"),
        ] {
            let document = LayoutDocument::from_toml(&module_document(kind)).expect("module TOML");
            assert_eq!(document.modules.len(), 1);
            let matches_expected = matches!(
                (&document.modules[0], expected),
                (ModuleDocument::Metric(_), "metric")
                    | (ModuleDocument::Sparkline(_), "sparkline")
                    | (ModuleDocument::Text(_), "text")
                    | (ModuleDocument::Media(_), "media")
            );
            assert!(matches_expected);
            let round_tripped =
                LayoutDocument::from_toml(&document.to_toml().expect("serialize module TOML"))
                    .expect("round trip module TOML");
            assert_eq!(round_tripped, document);
        }
    }

    #[test]
    fn rejects_unsupported_versions_without_rewriting_input() {
        let input = CANONICAL_TOML.replace("version = 1", "version = 2");
        let original = input.clone();
        let error = LayoutDocument::from_toml(&input).expect_err("version must be rejected");

        assert!(matches!(error, LayoutDocumentError::UnsupportedVersion(2)));
        assert_eq!(input, original);
    }

    #[test]
    fn serializing_unsupported_versions_is_rejected() {
        let document = LayoutDocument {
            version: CURRENT_VERSION + 1,
            name: "future".to_string(),
            preset: None,
            modules: Vec::new(),
            profiles: BTreeMap::new(),
        };

        assert!(matches!(
            document.to_toml(),
            Err(LayoutDocumentError::UnsupportedVersion(2))
        ));
    }
}
