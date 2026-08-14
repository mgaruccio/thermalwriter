//! Shared layout document types for the daemon and configuration GUI.

pub mod bindings;
pub mod diagnostic;
pub mod document;
pub mod media_cache;
pub mod modules;
pub mod persistence;
pub mod renderer;
pub mod scene;
pub mod solver;
pub mod surface;
pub mod svg_backend;
pub mod validation;

pub use bindings::{
    layout_binding_alias, layout_binding_is_known, layout_binding_label, published_layout_aliases,
    sensor_key_for_layout_binding,
};
pub use diagnostic::{DiagnosticSeverity, LayoutDiagnostic};
pub use document::{
    CURRENT_VERSION, LayoutDocument, LayoutDocumentError, MediaDocument, MetricDocument,
    ModuleDocument, ProfileRecipeDocument, SparklineDocument, TextDocument,
};
pub use media_cache::{
    MAX_MEDIA_ALLOC_BYTES, MAX_MEDIA_DIMENSION, MAX_MEDIA_FILE_BYTES, MEDIA_CACHE_DIAGNOSTIC_CODE,
    MediaCache, MediaCacheKey,
};
pub use modules::{
    BindingValue, HistoryBinding, MediaFit, MediaModule, MetricModule, MetricVariant,
    ModuleCapabilities, ModuleEmitter, ResolvedBindings, SparklineModule, SparklineStyle,
    SparklineVariant, TextModule, ThemeTokens, ValueRange,
};
pub use persistence::{
    LEGACY_LAYOUT_CODE, PERSISTENCE_CONFLICT_CODE, PERSISTENCE_DIAGNOSTIC_CODE,
    PERSISTENCE_PATH_CODE, SavedLayout, save_layout_document,
};
pub use renderer::{LayoutEngineRenderer, resolved_bindings};
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
