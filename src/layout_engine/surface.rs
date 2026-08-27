//! Explicit display-surface topology profiles for the layout engine.
//!
//! A surface profile is deliberately separate from aspect classification.  A
//! 2400x1080 surface is rectangular unless a caller explicitly selects the
//! [`SurfaceProfileId::ThermalrightCurved2400x1080`] profile.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable identifier for a display-surface topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum SurfaceProfileId {
    /// A single rectangular readable surface.
    #[serde(rename = "rectangular")]
    #[default]
    Rectangular,
    /// Thermalright's provisional 2400x1080 curved panorama topology.
    #[serde(rename = "thermalright-curved-2400x1080")]
    ThermalrightCurved2400x1080,
}

impl SurfaceProfileId {
    /// Stable kebab-case identifier used by layout documents and UI labels.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rectangular => "rectangular",
            Self::ThermalrightCurved2400x1080 => "thermalright-curved-2400x1080",
        }
    }
}

impl fmt::Display for SurfaceProfileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An axis-aligned rectangle in native surface pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SurfaceBounds {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl SurfaceBounds {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    pub const fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    /// Return whether two bounds overlap in their positive-area interiors.
    /// Edge contact is not overlap, which keeps protected boundaries usable.
    pub const fn overlaps(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    pub const fn contains(self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// A readable local area of a display surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceZone {
    /// Stable name used by layout diagnostics and preview overlays.
    pub name: &'static str,
    pub bounds: SurfaceBounds,
}

impl SurfaceZone {
    pub const fn new(name: &'static str, bounds: SurfaceBounds) -> Self {
        Self { name, bounds }
    }
}

/// A protected or otherwise non-readable area of a display surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceRegion {
    /// Stable name used by layout diagnostics and preview overlays.
    pub name: &'static str,
    pub bounds: SurfaceBounds,
}

impl SurfaceRegion {
    pub const fn new(name: &'static str, bounds: SurfaceBounds) -> Self {
        Self { name, bounds }
    }
}

/// The topology a preview should illustrate.
///
/// This describes the overlay/topology presentation only.  It does not claim
/// a calibrated optical projection, perspective transform, or mesh warp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreviewTopology {
    #[serde(rename = "rectangular")]
    Rectangular,
    #[serde(rename = "curved-panorama")]
    CurvedPanorama,
}

/// Static geometry and preview metadata for one display surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplaySurfaceProfile {
    pub id: SurfaceProfileId,
    pub width: u32,
    pub height: u32,
    pub readable_zones: &'static [SurfaceZone],
    pub protected_regions: &'static [SurfaceRegion],
    pub preview: PreviewTopology,
}

impl DisplaySurfaceProfile {
    pub const fn dimensions(self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub const fn is_curved(self) -> bool {
        matches!(self.preview, PreviewTopology::CurvedPanorama)
    }
}

static NO_PROTECTED_REGIONS: [SurfaceRegion; 0] = [];

static RECTANGULAR_480X480_ZONES: [SurfaceZone; 1] = [SurfaceZone::new(
    "full-surface",
    SurfaceBounds::new(0, 0, 480, 480),
)];
static RECTANGULAR_480X1280_ZONES: [SurfaceZone; 1] = [SurfaceZone::new(
    "full-surface",
    SurfaceBounds::new(0, 0, 480, 1280),
)];
static RECTANGULAR_1280X480_ZONES: [SurfaceZone; 1] = [SurfaceZone::new(
    "full-surface",
    SurfaceBounds::new(0, 0, 1280, 480),
)];
// This explicit rectangular counterpart is important: dimensions alone must
// not select the curved profile, even when they match its native resolution.
static RECTANGULAR_2400X1080_ZONES: [SurfaceZone; 1] = [SurfaceZone::new(
    "full-surface",
    SurfaceBounds::new(0, 0, 2400, 1080),
)];

static CURVED_READABLE_ZONES: [SurfaceZone; 2] = [
    SurfaceZone::new("left-readable", SurfaceBounds::new(0, 0, 960, 1080)),
    SurfaceZone::new("right-readable", SurfaceBounds::new(1440, 0, 960, 1080)),
];

// Provisional and intentionally conservative.  Thermalright does not publish
// calibrated bridge bounds or a curvature model; these bounds are a protected
// keep-out overlay, not an optical correction or hardware claim.
static CURVED_PROTECTED_REGIONS: [SurfaceRegion; 1] = [SurfaceRegion::new(
    "center-bridge",
    SurfaceBounds::new(960, 0, 480, 1080),
)];

static SURFACE_PROFILES: [DisplaySurfaceProfile; 5] = [
    DisplaySurfaceProfile {
        id: SurfaceProfileId::Rectangular,
        width: 480,
        height: 480,
        readable_zones: &RECTANGULAR_480X480_ZONES,
        protected_regions: &NO_PROTECTED_REGIONS,
        preview: PreviewTopology::Rectangular,
    },
    DisplaySurfaceProfile {
        id: SurfaceProfileId::Rectangular,
        width: 480,
        height: 1280,
        readable_zones: &RECTANGULAR_480X1280_ZONES,
        protected_regions: &NO_PROTECTED_REGIONS,
        preview: PreviewTopology::Rectangular,
    },
    DisplaySurfaceProfile {
        id: SurfaceProfileId::Rectangular,
        width: 1280,
        height: 480,
        readable_zones: &RECTANGULAR_1280X480_ZONES,
        protected_regions: &NO_PROTECTED_REGIONS,
        preview: PreviewTopology::Rectangular,
    },
    DisplaySurfaceProfile {
        id: SurfaceProfileId::Rectangular,
        width: 2400,
        height: 1080,
        readable_zones: &RECTANGULAR_2400X1080_ZONES,
        protected_regions: &NO_PROTECTED_REGIONS,
        preview: PreviewTopology::Rectangular,
    },
    DisplaySurfaceProfile {
        id: SurfaceProfileId::ThermalrightCurved2400x1080,
        width: 2400,
        height: 1080,
        readable_zones: &CURVED_READABLE_ZONES,
        protected_regions: &CURVED_PROTECTED_REGIONS,
        preview: PreviewTopology::CurvedPanorama,
    },
];

/// The static registry of supported surface-profile/dimension combinations.
#[derive(Debug, Clone, Copy, Default)]
pub struct SurfaceProfileRegistry;

impl SurfaceProfileRegistry {
    pub const fn new() -> Self {
        Self
    }

    /// Resolve dimensions only as an explicit rectangular profile.
    ///
    /// This is the safe path for unknown device identities: a matching
    /// resolution never implies curvature.
    pub fn rectangular(&self, width: u32, height: u32) -> Option<&'static DisplaySurfaceProfile> {
        self.resolve(width, height, SurfaceProfileId::Rectangular)
    }

    /// Resolve a profile only when both its dimensions and identifier match.
    pub fn resolve(
        &self,
        width: u32,
        height: u32,
        id: SurfaceProfileId,
    ) -> Option<&'static DisplaySurfaceProfile> {
        SURFACE_PROFILES
            .iter()
            .find(|profile| profile.width == width && profile.height == height && profile.id == id)
    }

    /// Return every bounded profile in the registry.
    pub fn profiles(&self) -> &'static [DisplaySurfaceProfile] {
        &SURFACE_PROFILES
    }
}

/// Shared static surface-profile registry.
pub static SURFACE_PROFILE_REGISTRY: SurfaceProfileRegistry = SurfaceProfileRegistry;

/// Resolve a profile by native dimensions and an explicit topology id.
pub fn resolve_surface_profile(
    width: u32,
    height: u32,
    id: SurfaceProfileId,
) -> Option<&'static DisplaySurfaceProfile> {
    SURFACE_PROFILE_REGISTRY.resolve(width, height, id)
}

/// Resolve a native resolution with the explicit rectangular default.
pub fn rectangular_surface_profile(
    width: u32,
    height: u32,
) -> Option<&'static DisplaySurfaceProfile> {
    SURFACE_PROFILE_REGISTRY.rectangular(width, height)
}

/// Return all bounded surface profiles.
pub fn known_surface_profiles() -> &'static [DisplaySurfaceProfile] {
    SURFACE_PROFILE_REGISTRY.profiles()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_rectangular_fixture_profiles() {
        for (width, height) in [(480, 480), (480, 1280), (1280, 480)] {
            let profile = rectangular_surface_profile(width, height).expect("fixture profile");
            assert_eq!(profile.id, SurfaceProfileId::Rectangular);
            assert_eq!(profile.dimensions(), (width, height));
            assert_eq!(profile.readable_zones.len(), 1);
            assert!(profile.protected_regions.is_empty());
            assert_eq!(profile.preview, PreviewTopology::Rectangular);
        }
    }

    #[test]
    fn curved_profile_has_two_readable_zones_and_one_bridge() {
        let profile =
            resolve_surface_profile(2400, 1080, SurfaceProfileId::ThermalrightCurved2400x1080)
                .expect("curved profile");

        assert_eq!(profile.readable_zones.len(), 2);
        assert_eq!(profile.protected_regions.len(), 1);
        assert_eq!(profile.preview, PreviewTopology::CurvedPanorama);

        let left = profile.readable_zones[0].bounds;
        let right = profile.readable_zones[1].bounds;
        let bridge = profile.protected_regions[0].bounds;
        assert_eq!(left.right(), bridge.x);
        assert_eq!(bridge.right(), right.x);
        assert_eq!(left.bottom(), 1080);
        assert_eq!(right.bottom(), 1080);
    }

    #[test]
    fn protected_bridge_edge_contact_is_safe_but_interior_overlap_is_not() {
        let left = SurfaceBounds::new(0, 0, 960, 1080);
        let bridge = SurfaceBounds::new(960, 0, 480, 1080);
        let interior = SurfaceBounds::new(959, 0, 481, 1080);

        assert!(!left.overlaps(bridge));
        assert!(interior.overlaps(bridge));
    }

    #[test]
    fn matching_dimensions_do_not_infer_curvature() {
        let rectangular = rectangular_surface_profile(2400, 1080).expect("rectangular fallback");
        assert_eq!(rectangular.id, SurfaceProfileId::Rectangular);
        assert!(!rectangular.is_curved());
        assert!(rectangular.protected_regions.is_empty());

        let curved =
            resolve_surface_profile(2400, 1080, SurfaceProfileId::ThermalrightCurved2400x1080)
                .expect("explicit curved profile");
        assert_eq!(curved.id, SurfaceProfileId::ThermalrightCurved2400x1080);
        assert!(curved.is_curved());
    }

    #[test]
    fn unknown_dimensions_remain_unresolved_without_implicit_geometry() {
        assert!(rectangular_surface_profile(1920, 1080).is_none());
        assert!(
            resolve_surface_profile(1920, 1080, SurfaceProfileId::ThermalrightCurved2400x1080)
                .is_none()
        );
    }
}
