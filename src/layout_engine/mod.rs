//! Shared layout document types for the daemon and configuration GUI.

pub mod diagnostic;
pub mod document;
pub mod solver;
pub mod surface;
pub mod validation;

pub use diagnostic::{DiagnosticSeverity, LayoutDiagnostic};
pub use document::{
    LayoutDocument, LayoutDocumentError, MediaDocument, MetricDocument, ModuleDocument,
    ProfileRecipeDocument, SparklineDocument, TextDocument, CURRENT_VERSION,
};
pub use solver::{
    solve, BridgePolicy, RecipeKind, Rect, SolvedLayout, SolvedModule, CARD_BASE_EXTENT,
    CARD_MIN_EXTENT, CONTENT_INSET_BASE, TOKEN_REFERENCE_AXIS,
};
pub use surface::{
    known_surface_profiles, rectangular_surface_profile, resolve_surface_profile,
    DisplaySurfaceProfile, PreviewTopology, SurfaceBounds, SurfaceProfileId,
    SurfaceProfileRegistry, SurfaceRegion, SurfaceZone, SURFACE_PROFILE_REGISTRY,
};
pub use validation::{
    validate, validate_rectangular_recipe, BRIDGE_VIOLATION_CODE, MODULE_ID_CODE,
    RECIPE_OVERFLOW_CODE, RECIPE_SHAPE_CODE, RECTANGULAR_SURFACE_CODE, UNKNOWN_RECIPE_CODE,
    UNSUPPORTED_RECIPE_CODE, ZONE_OVERFLOW_CODE,
};
