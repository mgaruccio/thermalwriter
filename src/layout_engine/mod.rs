//! Shared layout document types for the daemon and configuration GUI.

pub mod diagnostic;
pub mod document;
pub mod surface;

pub use diagnostic::{DiagnosticSeverity, LayoutDiagnostic};
pub use document::{
    CURRENT_VERSION, LayoutDocument, LayoutDocumentError, MediaDocument, MetricDocument,
    ModuleDocument, ProfileRecipeDocument, SparklineDocument, TextDocument,
};
pub use surface::{
    DisplaySurfaceProfile, PreviewTopology, SURFACE_PROFILE_REGISTRY, SurfaceBounds,
    SurfaceProfileId, SurfaceProfileRegistry, SurfaceRegion, SurfaceZone, known_surface_profiles,
    rectangular_surface_profile, resolve_surface_profile,
};
