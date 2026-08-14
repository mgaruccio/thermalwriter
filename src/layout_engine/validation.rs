//! Rectangular recipe validation for the shared layout engine.
//!
//! Validation is intentionally separate from placement.  It resolves the
//! aspect-class recipe, rejects unsupported topology, checks typed module IDs,
//! and proves that fixed card extents fit before the solver emits any bounds.
//! Owners can reorder modules, but cannot bypass these bounded checks by
//! entering coordinates.

use std::collections::BTreeSet;

use super::diagnostic::{DiagnosticSeverity, LayoutDiagnostic};
use super::document::LayoutDocument;
use super::solver::{
    CARD_MIN_EXTENT, RecipeKind, Rect, card_extent_for_surface, content_inset_for_surface,
    fixed_module_width,
};
use super::surface::DisplaySurfaceProfile;

/// Stable diagnostic for a recipe that this rectangular solver cannot solve.
pub const UNSUPPORTED_RECIPE_CODE: &str = "TWLAYOUT-E020";

/// Stable diagnostic for an aspect-incompatible recipe name.
pub const RECIPE_SHAPE_CODE: &str = "TWLAYOUT-E021";
/// Stable diagnostic for an unknown persisted recipe name.
pub const UNKNOWN_RECIPE_CODE: &str = "TWLAYOUT-E025";
/// Stable diagnostic for a fixed-card capacity or surface-fit failure.
pub const RECIPE_OVERFLOW_CODE: &str = "TWLAYOUT-E022";

/// Stable diagnostic for duplicate or empty typed module IDs.
pub const MODULE_ID_CODE: &str = "TWLAYOUT-E023";

/// Stable diagnostic for a surface that is not a single rectangular readable
/// region.
pub const RECTANGULAR_SURFACE_CODE: &str = "TWLAYOUT-E024";

/// Validate the rectangular recipe and fixed-card capacity for a document.
///
/// The returned recipe is always the aspect-class recipe: `column` for
/// `height >= width`, and `two-column` for `width > height`.  A matching
/// profile entry may make that choice explicit, but cannot replace it with a
/// different rectangular recipe.  `zoned-panorama` is recognized so that it
/// can produce one stable unsupported diagnostic rather than being treated as
/// an unknown string.
pub fn validate_rectangular_recipe(
    document: &LayoutDocument,
    surface: &DisplaySurfaceProfile,
) -> Result<RecipeKind, Vec<LayoutDiagnostic>> {
    let expected = aspect_recipe(surface);
    let configured = configured_recipe(document, surface)?;

    // Resolve the explicit unsupported branch before rectangular topology
    // checks so a requested panorama always gets the same stable recipe code,
    // including when the selected surface is itself curved.
    if configured == Some(RecipeKind::ZonedPanorama) {
        return Err(vec![unsupported_recipe_diagnostic(surface)]);
    }

    let bounds = rectangular_content_bounds(surface).map_err(|diagnostic| vec![diagnostic])?;

    if let Some(configured) = configured {
        if configured != expected {
            return Err(vec![shape_mismatch_diagnostic(
                surface, configured, expected,
            )]);
        }
    }

    let id_diagnostics = validate_module_ids(document);
    if !id_diagnostics.is_empty() {
        return Err(id_diagnostics);
    }

    let card_extent = card_extent_for_surface(surface);
    let module_width = match expected {
        RecipeKind::Column => bounds.width,
        RecipeKind::TwoColumn => fixed_module_width(surface),
        RecipeKind::ZonedPanorama => unreachable!("aspect recipes are rectangular"),
    };

    if !document.modules.is_empty() && module_width < CARD_MIN_EXTENT {
        return Err(vec![overflow_diagnostic(
            surface,
            expected,
            format!(
                "the fixed module width is {module_width}px, below the typed minimum of {CARD_MIN_EXTENT}px"
            ),
            format!(
                "Increase the readable width or choose a rectangular surface with at least {CARD_MIN_EXTENT}px of content width."
            ),
        )]);
    }

    if expected == RecipeKind::TwoColumn
        && !document.modules.is_empty()
        && u64::from(module_width) * 2 > u64::from(bounds.width)
    {
        return Err(vec![overflow_diagnostic(
            surface,
            expected,
            format!(
                "two fixed {module_width}px columns exceed the {width}px content width",
                width = bounds.width
            ),
            "Use a wider surface or reduce the module count; cards are never shrunk to fit."
                .to_owned(),
        )]);
    }

    let rows = match expected {
        RecipeKind::Column => document.modules.len(),
        RecipeKind::TwoColumn => document.modules.len().div_ceil(2),
        RecipeKind::ZonedPanorama => unreachable!("aspect recipes are rectangular"),
    };
    let capacity = if card_extent == 0 {
        0
    } else {
        usize::try_from(u64::from(bounds.height) / u64::from(card_extent)).unwrap_or(usize::MAX)
    };
    let capacity = match expected {
        RecipeKind::Column => capacity,
        RecipeKind::TwoColumn => capacity.saturating_mul(2),
        RecipeKind::ZonedPanorama => 0,
    };

    if rows
        > if expected == RecipeKind::TwoColumn {
            capacity / 2
        } else {
            capacity
        }
    {
        return Err(vec![overflow_diagnostic(
            surface,
            expected,
            format!(
                "{} fixed {card_extent}px card rows require more than capacity {capacity}",
                rows
            ),
            format!(
                "Remove or reorder modules so this surface uses at most {capacity} module(s); cards are never shrunk or overlapped."
            ),
        )]);
    }

    Ok(expected)
}

/// Short alias for callers that do not need to spell out the rectangular
/// boundary.  It intentionally has the same result as
/// [`validate_rectangular_recipe`].
pub fn validate(
    document: &LayoutDocument,
    surface: &DisplaySurfaceProfile,
) -> Result<RecipeKind, Vec<LayoutDiagnostic>> {
    validate_rectangular_recipe(document, surface)
}

/// Resolve the inset content rectangle for a rectangular surface.
///
/// This is crate-visible so the solver and validator share exactly one set of
/// surface checks and cannot drift on edge handling.
pub(crate) fn rectangular_content_bounds(
    surface: &DisplaySurfaceProfile,
) -> Result<Rect, LayoutDiagnostic> {
    if surface.width == 0 || surface.height == 0 {
        return Err(surface_diagnostic(
            surface,
            "A rectangular layout needs a non-zero surface size.",
            "The selected display profile has a zero width or height.",
            "Select a bounded rectangular display profile with non-zero dimensions.",
        ));
    }
    if surface.is_curved() {
        return Err(surface_diagnostic(
            surface,
            "Curved layout solving is not supported in this task.",
            "The selected surface advertises curved-panorama topology.",
            "Select a rectangular surface or use the future zoned-panorama solver.",
        ));
    }
    if !surface.protected_regions.is_empty() || surface.readable_zones.len() != 1 {
        return Err(surface_diagnostic(
            surface,
            "The rectangular recipe requires one complete readable surface.",
            "Protected regions or multiple readable zones require topology-aware placement.",
            "Select a single-zone rectangular profile for this solver.",
        ));
    }

    let zone = surface.readable_zones[0];
    if zone.bounds.x != 0
        || zone.bounds.y != 0
        || zone.bounds.width != surface.width
        || zone.bounds.height != surface.height
    {
        return Err(surface_diagnostic(
            surface,
            "The rectangular recipe requires a full-surface readable zone.",
            "The selected readable zone does not cover the complete rectangular profile.",
            "Use a profile whose only readable zone spans the native surface bounds.",
        ));
    }

    let inset = content_inset_for_surface(surface);
    let double_inset = inset.saturating_mul(2);
    if surface.width <= double_inset || surface.height <= double_inset {
        return Err(surface_diagnostic(
            surface,
            "The rectangular content area is too small for typed modules.",
            format!(
                "The responsive content inset is {inset}px on each edge of a {}x{} surface.",
                surface.width, surface.height
            ),
            format!(
                "Use a surface with more than {double_inset}px on both axes before placing modules."
            ),
        ));
    }

    Ok(Rect::new(
        inset,
        inset,
        surface.width - double_inset,
        surface.height - double_inset,
    ))
}

fn aspect_recipe(surface: &DisplaySurfaceProfile) -> RecipeKind {
    if surface.height >= surface.width {
        RecipeKind::Column
    } else {
        RecipeKind::TwoColumn
    }
}

fn aspect_name(surface: &DisplaySurfaceProfile) -> &'static str {
    if surface.width == surface.height {
        "square"
    } else if surface.height > surface.width {
        "portrait"
    } else {
        "wide"
    }
}

fn configured_recipe(
    document: &LayoutDocument,
    surface: &DisplaySurfaceProfile,
) -> Result<Option<RecipeKind>, Vec<LayoutDiagnostic>> {
    let aspect = aspect_name(surface);
    let profile = document
        .profiles
        .get(surface.id.as_str())
        .or_else(|| document.profiles.get(aspect));
    let Some(profile) = profile else {
        return Ok(None);
    };
    let Some(recipe) = RecipeKind::parse(profile.recipe.trim()) else {
        return Err(vec![unknown_recipe_diagnostic(
            surface,
            profile.recipe.trim(),
        )]);
    };
    Ok(Some(recipe))
}

fn validate_module_ids(document: &LayoutDocument) -> Vec<LayoutDiagnostic> {
    let mut seen = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for module in &document.modules {
        let id = module_id(module);
        if id.trim().is_empty() {
            diagnostics.push(module_id_diagnostic(
                id,
                "A typed module ID must not be empty.",
                "Give every module a stable non-empty ID so GUI and daemon results can be matched.",
            ));
        } else if !seen.insert(id) {
            diagnostics.push(module_id_diagnostic(
                id,
                "Duplicate typed module ID.",
                "Rename the duplicate module so every solved module has a unique stable ID.",
            ));
        }
    }
    diagnostics
}

fn module_id(module: &super::document::ModuleDocument) -> &str {
    match module {
        super::document::ModuleDocument::Metric(module) => &module.id,
        super::document::ModuleDocument::Sparkline(module) => &module.id,
        super::document::ModuleDocument::Text(module) => &module.id,
        super::document::ModuleDocument::Media(module) => &module.id,
    }
}

fn surface_diagnostic(
    surface: &DisplaySurfaceProfile,
    message: impl Into<String>,
    reason: impl Into<String>,
    fix: impl Into<String>,
) -> LayoutDiagnostic {
    let mut diagnostic = LayoutDiagnostic::new(
        RECTANGULAR_SURFACE_CODE,
        DiagnosticSeverity::Error,
        message,
        reason,
        fix,
    );
    diagnostic.profile = Some(aspect_name(surface).to_owned());
    diagnostic
}

fn unsupported_recipe_diagnostic(surface: &DisplaySurfaceProfile) -> LayoutDiagnostic {
    let mut diagnostic = LayoutDiagnostic::new(
        UNSUPPORTED_RECIPE_CODE,
        DiagnosticSeverity::Error,
        "Unsupported layout recipe",
        "Recipe `zoned-panorama` is not supported in this task.",
        "Select `column` or `two-column` for a rectangular surface.",
    );
    diagnostic.profile = Some(aspect_name(surface).to_owned());
    diagnostic.property_path = Some("recipe".to_owned());
    diagnostic
}

fn unknown_recipe_diagnostic(surface: &DisplaySurfaceProfile, recipe: &str) -> LayoutDiagnostic {
    let mut diagnostic = LayoutDiagnostic::new(
        UNKNOWN_RECIPE_CODE,
        DiagnosticSeverity::Error,
        "Unknown layout recipe",
        format!("Recipe `{recipe}` is not one of the supported typed recipes."),
        "Use `column`, `two-column`, or the explicitly reserved `zoned-panorama` recipe name.",
    );
    diagnostic.profile = Some(aspect_name(surface).to_owned());
    diagnostic.property_path = Some("recipe".to_owned());
    diagnostic
}

fn shape_mismatch_diagnostic(
    surface: &DisplaySurfaceProfile,
    configured: RecipeKind,
    expected: RecipeKind,
) -> LayoutDiagnostic {
    let mut diagnostic = LayoutDiagnostic::new(
        RECIPE_SHAPE_CODE,
        DiagnosticSeverity::Error,
        "Recipe does not match the rectangular surface shape",
        format!(
            "Recipe `{configured}` was selected, but a {}x{} surface requires `{expected}`.",
            surface.width, surface.height
        ),
        format!("Use recipe `{expected}` for the selected surface shape."),
    );
    diagnostic.profile = Some(aspect_name(surface).to_owned());
    diagnostic.property_path = Some("recipe".to_owned());
    diagnostic
}

fn overflow_diagnostic(
    surface: &DisplaySurfaceProfile,
    recipe: RecipeKind,
    reason: String,
    fix: String,
) -> LayoutDiagnostic {
    let mut diagnostic = LayoutDiagnostic::new(
        RECIPE_OVERFLOW_CODE,
        DiagnosticSeverity::Error,
        "Layout recipe capacity exceeded",
        format!("Recipe `{recipe}` cannot place modules without shrinking or overlap: {reason}."),
        fix,
    );
    diagnostic.profile = Some(aspect_name(surface).to_owned());
    diagnostic.property_path = Some("modules".to_owned());
    diagnostic
}

fn module_id_diagnostic(id: &str, message: &str, fix: &str) -> LayoutDiagnostic {
    let mut diagnostic = LayoutDiagnostic::new(
        MODULE_ID_CODE,
        DiagnosticSeverity::Error,
        message,
        format!("Module ID `{id}` is not unique and stable."),
        fix,
    );
    diagnostic.module_id = (!id.is_empty()).then_some(id.to_owned());
    diagnostic.property_path = Some("modules[].id".to_owned());
    diagnostic
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::layout_engine::document::{
        CURRENT_VERSION, LayoutDocument, MetricDocument, ModuleDocument, ProfileRecipeDocument,
    };
    use crate::layout_engine::surface::rectangular_surface_profile;

    fn document(recipe: Option<(&str, &str)>, ids: &[&str]) -> LayoutDocument {
        LayoutDocument {
            version: CURRENT_VERSION,
            name: "validation-test".to_owned(),
            preset: None,
            modules: ids
                .iter()
                .map(|id| {
                    ModuleDocument::Metric(MetricDocument {
                        id: (*id).to_owned(),
                        binding: "sensor.value".to_owned(),
                        variant: "default".to_owned(),
                    })
                })
                .collect(),
            profiles: recipe
                .map(|(name, recipe)| {
                    (
                        name.to_owned(),
                        ProfileRecipeDocument {
                            recipe: recipe.to_owned(),
                            bridge: None,
                        },
                    )
                })
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn aspect_class_defaults_cover_square_portrait_and_wide() {
        for (width, height, expected) in [
            (480, 480, RecipeKind::Column),
            (480, 1280, RecipeKind::Column),
            (1280, 480, RecipeKind::TwoColumn),
        ] {
            let surface = rectangular_surface_profile(width, height).expect("fixture");
            assert_eq!(
                validate_rectangular_recipe(&document(None, &["module"]), surface)
                    .expect("valid recipe"),
                expected
            );
        }
    }

    #[test]
    fn an_explicit_shape_mismatch_is_rejected_instead_of_reinterpreted() {
        let surface = rectangular_surface_profile(1280, 480).expect("wide fixture");
        let error =
            validate_rectangular_recipe(&document(Some(("wide", "column")), &["module"]), surface)
                .expect_err("column cannot override wide");
        assert_eq!(error[0].code, RECIPE_SHAPE_CODE);
        assert!(error[0].reason.contains("requires `two-column`"));
    }

    #[test]
    fn duplicate_ids_are_rejected_before_solving() {
        let surface = rectangular_surface_profile(480, 480).expect("square fixture");
        let error =
            validate_rectangular_recipe(&document(None, &["duplicate", "duplicate"]), surface)
                .expect_err("duplicate ID");
        assert_eq!(error[0].code, MODULE_ID_CODE);
        assert_eq!(error[0].module_id.as_deref(), Some("duplicate"));
    }
}
