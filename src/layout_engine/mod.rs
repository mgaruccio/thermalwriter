//! Shared layout document types for the daemon and configuration GUI.

pub mod diagnostic;
pub mod document;
pub mod solver;
pub mod surface;
pub mod validation;

pub use diagnostic::{DiagnosticSeverity, LayoutDiagnostic};
pub use document::{
    CURRENT_VERSION, LayoutDocument, LayoutDocumentError, MediaDocument, MetricDocument,
    ModuleDocument, ProfileRecipeDocument, SparklineDocument, TextDocument,
};
pub use solver::{
    CARD_BASE_EXTENT, CARD_MIN_EXTENT, CONTENT_INSET_BASE, RecipeKind, Rect, SolvedLayout,
    SolvedModule, TOKEN_REFERENCE_AXIS, solve,
};
pub use surface::{
    DisplaySurfaceProfile, PreviewTopology, SURFACE_PROFILE_REGISTRY, SurfaceBounds,
    SurfaceProfileId, SurfaceProfileRegistry, SurfaceRegion, SurfaceZone, known_surface_profiles,
    rectangular_surface_profile, resolve_surface_profile,
};
pub use validation::{
    MODULE_ID_CODE, RECIPE_OVERFLOW_CODE, RECIPE_SHAPE_CODE, RECTANGULAR_SURFACE_CODE,
    UNKNOWN_RECIPE_CODE, UNSUPPORTED_RECIPE_CODE, validate, validate_rectangular_recipe,
};
