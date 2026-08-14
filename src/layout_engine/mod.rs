//! Shared layout document types for the daemon and configuration GUI.

pub mod diagnostic;
pub mod document;
pub mod modules;
pub mod scene;
pub mod solver;
pub mod surface;
pub mod svg_backend;
pub mod validation;

pub use diagnostic::{DiagnosticSeverity, LayoutDiagnostic};
pub use document::{
    CURRENT_VERSION, LayoutDocument, LayoutDocumentError, MediaDocument, MetricDocument,
    ModuleDocument, ProfileRecipeDocument, SparklineDocument, TextDocument,
};
pub use modules::{
    BindingValue, HistoryBinding, MediaFit, MediaModule, MetricModule, MetricVariant,
    ModuleCapabilities, ModuleEmitter, ResolvedBindings, SparklineModule, SparklineStyle,
    SparklineVariant, TextModule, ThemeTokens, ValueRange,
};
pub use scene::{
    ClipNode, Color, ImageFit, ImageNode, MIN_FOREGROUND_CHANNEL, MIN_OPACITY, MIN_TEXT_SIZE,
    PathNode, Point, RectNode, Scene, SceneNode, TextAlignment, TextNode, TextRole,
};
pub use solver::{
    BridgePolicy, CARD_BASE_EXTENT, CARD_MIN_EXTENT, CONTENT_INSET_BASE, RecipeKind, Rect,
    SolvedLayout, SolvedModule, TOKEN_REFERENCE_AXIS, solve,
};
pub use surface::{
    DisplaySurfaceProfile, PreviewTopology, SURFACE_PROFILE_REGISTRY, SurfaceBounds,
    SurfaceProfileId, SurfaceProfileRegistry, SurfaceRegion, SurfaceZone, known_surface_profiles,
    rectangular_surface_profile, resolve_surface_profile,
};
pub use svg_backend::{
    MediaAsset, ResolvedMedia, ResvgSceneBackend, SVG_BACKEND_DIAGNOSTIC_CODE, SceneBackend,
    compile_scene_xml,
};
pub use validation::{
    BRIDGE_VIOLATION_CODE, MODULE_ID_CODE, RECIPE_OVERFLOW_CODE, RECIPE_SHAPE_CODE,
    RECTANGULAR_SURFACE_CODE, UNKNOWN_RECIPE_CODE, UNSUPPORTED_RECIPE_CODE, ZONE_OVERFLOW_CODE,
    validate, validate_rectangular_recipe,
};
