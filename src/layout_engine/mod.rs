//! Shared layout document types for the daemon and configuration GUI.

pub mod document;

pub mod diagnostic;

pub use document::{
    CURRENT_VERSION, LayoutDocument, LayoutDocumentError, MediaDocument, MetricDocument,
    ModuleDocument, ProfileRecipeDocument, SparklineDocument, TextDocument,
};

pub use diagnostic::{DiagnosticSeverity, LayoutDiagnostic};
