//! Deterministic layout recipes for rectangular and curved-panorama surfaces.
//!
//! The solver deliberately keeps the authoring model small: modules are ordered
//! in the document and a recipe places them on bounded surface regions. It never
//! accepts coordinates from an owner, grows a card to consume leftover space, or
//! guesses at curved-display geometry.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::diagnostic::LayoutDiagnostic;
use super::document::{LayoutDocument, ModuleDocument};
use super::surface::{DisplaySurfaceProfile, SurfaceZone};
use super::validation;

/// The base content inset used by the 480-pixel display fixtures.
///
/// The inset follows the existing responsive `token_margin` rule: it scales
/// with the short axis and has an 8-pixel floor.  At 480 pixels the inset is
/// 16 pixels, leaving a 448-pixel content track.
pub const CONTENT_INSET_BASE: u32 = 16;

/// The short-axis reference used for responsive layout tokens.
pub const TOKEN_REFERENCE_AXIS: u32 = 480;

/// The historical card flow extent at the 480-pixel reference axis.
///
/// All current typed modules use this as their fixed minimum extent along the
/// recipe's flow axis.  The value scales with the short axis, with
/// [`CARD_MIN_EXTENT`] as its floor; it is never increased to consume leftover
/// room on a particular surface.
pub const CARD_BASE_EXTENT: u32 = 172;

/// The smallest supported flow extent for a typed module.
///
/// Keeping an explicit floor prevents a narrow rectangular fixture from
/// silently shrinking text, metric, sparkline, or media modules below a usable
/// card size.  A fixture that cannot hold this extent returns a diagnostic.
pub const CARD_MIN_EXTENT: u32 = 120;

/// Stable recipe names persisted in [`super::document::ProfileRecipeDocument`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecipeKind {
    /// Stack fixed-height modules along the vertical axis.
    Column,
    /// Place fixed-width modules in two horizontal tracks, adding rows as
    /// needed while preserving document order.
    TwoColumn,
    /// Place ordered modules into the readable zones of a curved panorama.
    ZonedPanorama,
}

/// Policy controlling which typed modules may span a protected panorama bridge.
///
/// A policy is only one half of the spanning decision: the module must also
/// advertise the corresponding capability. The current typed catalog grants that
/// capability to media modules; metrics, sparklines, and text remain local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BridgePolicy {
    /// Keep every module inside one readable zone.
    LocalOnly,
    /// Permit capable media modules to span when explicitly selected.
    MediaOnly,
    /// Permit any future module with an explicit bridge capability to span.
    ExplicitCapable,
}

impl BridgePolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalOnly => "local-only",
            Self::MediaOnly => "media-only",
            Self::ExplicitCapable => "explicit-capable",
        }
    }

    pub(crate) const fn can_span(self, module: &ModuleDocument) -> bool {
        match self {
            Self::LocalOnly => false,
            Self::MediaOnly | Self::ExplicitCapable => matches!(module, ModuleDocument::Media(_)),
        }
    }
}

impl fmt::Display for BridgePolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl RecipeKind {
    /// Return the stable kebab-case recipe name used by layout documents.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Column => "column",
            Self::TwoColumn => "two-column",
            Self::ZonedPanorama => "zoned-panorama",
        }
    }

    /// Parse a persisted recipe name without accepting ad-hoc coordinates or
    /// other layout languages.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "column" => Some(Self::Column),
            "two-column" => Some(Self::TwoColumn),
            "zoned-panorama" => Some(Self::ZonedPanorama),
            _ => None,
        }
    }
}

impl fmt::Display for RecipeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An axis-aligned rectangle in native surface pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
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

    /// Return whether two rectangles overlap in their positive-area interiors.
    pub const fn overlaps(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    pub const fn contains(self, other: Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }
}

/// One module after deterministic recipe placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SolvedModule {
    pub id: String,
    pub bounds: Rect,
    /// Rectangular recipes use the complete readable surface and therefore do
    /// not need a named zone.  The field is reserved for the future
    /// zoned-panorama solver.
    pub zone: Option<String>,
}

/// A complete, backend-neutral rectangular solve result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SolvedLayout {
    pub recipe: RecipeKind,
    /// The inset content bounds used for placement, not a generated pixel
    /// table.  Every solved module is contained by these bounds.
    pub bounds: Rect,
    pub modules: Vec<SolvedModule>,
}

/// Solve a typed layout document for a bounded display profile.
///
/// Square and portrait surfaces select [`RecipeKind::Column`]; wide surfaces
/// select [`RecipeKind::TwoColumn`]. An explicitly configured curved profile
/// selects [`RecipeKind::ZonedPanorama`], preserving local module order while
/// keeping non-spanning modules out of its protected bridge.
pub fn solve(
    document: &LayoutDocument,
    surface: &DisplaySurfaceProfile,
) -> Result<SolvedLayout, Vec<LayoutDiagnostic>> {
    let recipe = validation::validate(document, surface)?;
    let (bounds, modules) = match recipe {
        RecipeKind::Column => {
            let bounds = validation::rectangular_content_bounds(surface)
                .expect("validated rectangular surface must provide content bounds");
            let card_extent = card_extent_for_surface(surface);
            (bounds, solve_column(document, bounds, card_extent))
        }
        RecipeKind::TwoColumn => {
            let bounds = validation::rectangular_content_bounds(surface)
                .expect("validated rectangular surface must provide content bounds");
            let card_extent = card_extent_for_surface(surface);
            (
                bounds,
                solve_two_column(document, surface, bounds, card_extent),
            )
        }
        RecipeKind::ZonedPanorama => {
            let bounds = Rect::new(0, 0, surface.width, surface.height);
            (bounds, solve_zoned_panorama(document, surface)?)
        }
    };

    Ok(SolvedLayout {
        recipe,
        bounds,
        modules,
    })
}

fn solve_zoned_panorama(
    document: &LayoutDocument,
    surface: &DisplaySurfaceProfile,
) -> Result<Vec<SolvedModule>, Vec<LayoutDiagnostic>> {
    let policy = validation::panorama_bridge_policy(document, surface)
        .map_err(|diagnostic| vec![diagnostic])?;
    let zones = ordered_panorama_zones(surface);
    let card_extent = card_extent_for_surface(surface);
    let mut assignments = [Vec::new(), Vec::new()];
    let mut solved = vec![None; document.modules.len()];
    let mut local_index = 0usize;

    for (module_index, module) in document.modules.iter().enumerate() {
        if policy.can_span(module) {
            solved[module_index] = Some(SolvedModule {
                id: module_id(module).to_owned(),
                bounds: Rect::new(0, 0, surface.width, surface.height),
                zone: None,
            });
        } else {
            assignments[local_index % zones.len()].push(module_index);
            local_index += 1;
        }
    }

    for (zone_index, zone) in zones.iter().enumerate() {
        let assignment = &assignments[zone_index];
        let capacity = validation::panorama_zone_capacity(surface, *zone, card_extent);
        if assignment.len() > capacity {
            return Err(vec![validation::zone_overflow_diagnostic(
                surface,
                zone.name,
                assignment.len(),
                capacity,
            )]);
        }
    }

    for (zone_index, zone) in zones.iter().enumerate() {
        let zone_bounds = validation::panorama_zone_content_bounds(surface, *zone);
        let positions = space_between_positions(
            zone_bounds.y,
            zone_bounds.height,
            card_extent,
            assignments[zone_index].len(),
        );
        for (&module_index, y) in assignments[zone_index].iter().zip(positions) {
            let module = &document.modules[module_index];
            solved[module_index] = Some(SolvedModule {
                id: module_id(module).to_owned(),
                bounds: Rect::new(zone_bounds.x, y, zone_bounds.width, card_extent),
                zone: Some(zone.name.to_owned()),
            });
        }
    }

    Ok(solved
        .into_iter()
        .map(|module| module.expect("validated panorama assignment is complete"))
        .collect())
}

/// Return the two panorama zones in physical left-to-right order.
pub(crate) fn ordered_panorama_zones(surface: &DisplaySurfaceProfile) -> [SurfaceZone; 2] {
    let mut zones = [surface.readable_zones[0], surface.readable_zones[1]];
    if zones[1].bounds.x < zones[0].bounds.x {
        zones.swap(0, 1);
    }
    zones
}
fn solve_column(document: &LayoutDocument, bounds: Rect, card_extent: u32) -> Vec<SolvedModule> {
    let positions =
        space_between_positions(bounds.y, bounds.height, card_extent, document.modules.len());
    document
        .modules
        .iter()
        .zip(positions)
        .map(|(module, y)| SolvedModule {
            id: module_id(module).to_owned(),
            bounds: Rect::new(bounds.x, y, bounds.width, card_extent),
            zone: None,
        })
        .collect()
}

fn solve_two_column(
    document: &LayoutDocument,
    surface: &DisplaySurfaceProfile,
    bounds: Rect,
    card_extent: u32,
) -> Vec<SolvedModule> {
    let card_width = fixed_module_width(surface);
    let row_count = document.modules.len().div_ceil(2);
    let y_positions = space_between_positions(bounds.y, bounds.height, card_extent, row_count);
    let x_positions = space_between_positions(bounds.x, bounds.width, card_width, 2);

    document
        .modules
        .iter()
        .enumerate()
        .map(|(index, module)| {
            let row = index / 2;
            let column = index % 2;
            let x = if column == 1 && (row * 2 + 1) < document.modules.len() {
                x_positions[1]
            } else {
                x_positions[0]
            };
            SolvedModule {
                id: module_id(module).to_owned(),
                bounds: Rect::new(x, y_positions[row], card_width, card_extent),
                zone: None,
            }
        })
        .collect()
}

fn module_id(module: &ModuleDocument) -> &str {
    match module {
        ModuleDocument::Metric(module) => &module.id,
        ModuleDocument::Sparkline(module) => &module.id,
        ModuleDocument::Text(module) => &module.id,
        ModuleDocument::Media(module) => &module.id,
    }
}

/// Responsive inset derived from the short axis, matching the existing
/// `token_margin` convention without introducing per-resolution pixel tables.
pub(crate) fn content_inset_for_surface(surface: &DisplaySurfaceProfile) -> u32 {
    let short = surface.width.min(surface.height);
    ((short + 29) / 30).max(CONTENT_INSET_BASE / 2)
}

/// Responsive fixed flow extent derived from the short axis.
pub(crate) fn card_extent_for_surface(surface: &DisplaySurfaceProfile) -> u32 {
    let short = u64::from(surface.width.min(surface.height));
    let scaled = (short * u64::from(CARD_BASE_EXTENT) + u64::from(TOKEN_REFERENCE_AXIS / 2))
        / u64::from(TOKEN_REFERENCE_AXIS);
    u32::try_from(scaled)
        .unwrap_or(u32::MAX)
        .max(CARD_MIN_EXTENT)
}

/// Fixed width of a two-column card track.  It is one short-axis content
/// track, so a 1280x480 fixture uses 448-pixel cards and distributes the
/// remaining wide-axis space as the horizontal gap.
pub(crate) fn fixed_module_width(surface: &DisplaySurfaceProfile) -> u32 {
    let short = surface.width.min(surface.height);
    short.saturating_sub(content_inset_for_surface(surface).saturating_mul(2))
}

/// Integer equivalent of CSS `space-between` for non-negative coordinates.
/// Remainders are assigned by rounding each ideal position, which keeps the
/// final item exactly on the ending content edge and is stable across runs.
pub(crate) fn space_between_positions(
    origin: u32,
    span: u32,
    item_extent: u32,
    count: usize,
) -> Vec<u32> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![origin];
    }

    let count_u64 = count as u64;
    let leftover = u64::from(span).saturating_sub(u64::from(item_extent) * count_u64);
    let denominator = count_u64 - 1;
    (0..count)
        .map(|index| {
            let index_u64 = index as u64;
            let position = u64::from(origin)
                + u64::from(item_extent) * index_u64
                + (index_u64 * leftover + denominator / 2) / denominator;
            u32::try_from(position).unwrap_or(u32::MAX)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_engine::document::{
        CURRENT_VERSION, MediaDocument, MetricDocument, ProfileRecipeDocument, TextDocument,
    };
    use crate::layout_engine::surface::{
        DisplaySurfaceProfile, PreviewTopology, SurfaceBounds, SurfaceProfileId, SurfaceRegion,
        SurfaceZone, rectangular_surface_profile, resolve_surface_profile,
    };
    use std::collections::BTreeMap;

    fn document(ids: &[&str], recipe: Option<(&str, &str)>) -> LayoutDocument {
        let modules = ids
            .iter()
            .map(|id| {
                ModuleDocument::Metric(MetricDocument {
                    id: (*id).to_owned(),
                    binding: format!("{id}.value"),
                    variant: "default".to_owned(),
                })
            })
            .collect();
        let profiles = recipe
            .map(|(profile, recipe)| {
                (
                    profile.to_owned(),
                    ProfileRecipeDocument {
                        recipe: recipe.to_owned(),
                        bridge: None,
                    },
                )
            })
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        LayoutDocument {
            version: CURRENT_VERSION,
            name: "test-layout".to_owned(),
            preset: None,
            modules,
            profiles,
        }
    }

    fn curved_document(modules: Vec<ModuleDocument>, bridge: Option<&str>) -> LayoutDocument {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            SurfaceProfileId::ThermalrightCurved2400x1080
                .as_str()
                .to_owned(),
            ProfileRecipeDocument {
                recipe: "zoned-panorama".to_owned(),
                bridge: bridge.map(str::to_owned),
            },
        );
        LayoutDocument {
            version: CURRENT_VERSION,
            name: "curved-test".to_owned(),
            preset: None,
            modules,
            profiles,
        }
    }

    fn metric(id: &str) -> ModuleDocument {
        ModuleDocument::Metric(MetricDocument {
            id: id.to_owned(),
            binding: format!("{id}.value"),
            variant: "default".to_owned(),
        })
    }

    fn media(id: &str) -> ModuleDocument {
        ModuleDocument::Media(MediaDocument {
            id: id.to_owned(),
            binding: format!("{id}.source"),
            variant: "default".to_owned(),
            ..MediaDocument::default()
        })
    }

    fn text(id: &str) -> ModuleDocument {
        ModuleDocument::Text(TextDocument {
            id: id.to_owned(),
            binding: format!("{id}.text"),
            variant: "default".to_owned(),
        })
    }

    #[test]
    fn curved_panorama_assigns_ordered_modules_to_both_zones() {
        let surface =
            resolve_surface_profile(2400, 1080, SurfaceProfileId::ThermalrightCurved2400x1080)
                .expect("curved fixture");
        let layout = solve(
            &curved_document(
                vec![
                    metric("one"),
                    metric("two"),
                    metric("three"),
                    metric("four"),
                ],
                None,
            ),
            surface,
        )
        .expect("curved solve");

        assert_eq!(layout.recipe, RecipeKind::ZonedPanorama);
        assert_eq!(layout.bounds, Rect::new(0, 0, 2400, 1080));
        assert_eq!(
            layout
                .modules
                .iter()
                .map(|module| (module.id.as_str(), module.zone.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("one", Some("left-readable")),
                ("two", Some("right-readable")),
                ("three", Some("left-readable")),
                ("four", Some("right-readable")),
            ],
        );
        assert!(
            layout
                .modules
                .iter()
                .all(|module| { module.bounds.x < 960 || module.bounds.x >= 1440 })
        );
    }

    #[test]
    fn curved_panorama_normalizes_reversed_zone_declaration_order() {
        static ZONES: [SurfaceZone; 2] = [
            SurfaceZone::new("right-readable", SurfaceBounds::new(1440, 0, 960, 1080)),
            SurfaceZone::new("left-readable", SurfaceBounds::new(0, 0, 960, 1080)),
        ];
        static BRIDGE: [SurfaceRegion; 1] = [SurfaceRegion::new(
            "center-bridge",
            SurfaceBounds::new(960, 0, 480, 1080),
        )];
        static SURFACE: DisplaySurfaceProfile = DisplaySurfaceProfile {
            id: SurfaceProfileId::ThermalrightCurved2400x1080,
            width: 2400,
            height: 1080,
            readable_zones: &ZONES,
            protected_regions: &BRIDGE,
            preview: PreviewTopology::CurvedPanorama,
        };

        let layout = solve(
            &curved_document(vec![metric("left-first"), metric("right-second")], None),
            &SURFACE,
        )
        .expect("reversed zones still solve");
        assert_eq!(layout.modules[0].zone.as_deref(), Some("left-readable"));
        assert_eq!(layout.modules[1].zone.as_deref(), Some("right-readable"));
    }

    #[test]
    fn capable_media_spans_only_with_explicit_opt_in() {
        let surface =
            resolve_surface_profile(2400, 1080, SurfaceProfileId::ThermalrightCurved2400x1080)
                .expect("curved fixture");
        let spanning = solve(
            &curved_document(
                vec![media("wallpaper"), metric("metric"), text("label")],
                Some("media-only"),
            ),
            surface,
        )
        .expect("media span solve");
        assert_eq!(spanning.modules[0].bounds, Rect::new(0, 0, 2400, 1080));
        assert_eq!(spanning.modules[0].zone, None);
        assert!(spanning.modules[1].bounds.x < 960);
        assert!(spanning.modules[2].bounds.x >= 1440);

        let local = solve(
            &curved_document(
                vec![media("wallpaper"), metric("metric"), text("label")],
                None,
            ),
            surface,
        )
        .expect("local media solve");
        assert!(local.modules[0].bounds.x < 960);
        assert_eq!(local.modules[0].zone.as_deref(), Some("left-readable"));
    }

    #[test]
    fn unknown_bridge_policy_uses_new_stable_diagnostic() {
        let surface =
            resolve_surface_profile(2400, 1080, SurfaceProfileId::ThermalrightCurved2400x1080)
                .expect("curved fixture");
        let error = solve(
            &curved_document(vec![metric("metric")], Some("all-modules")),
            surface,
        )
        .expect_err("unknown bridge policy must be rejected");
        assert_eq!(error[0].code, validation::BRIDGE_VIOLATION_CODE);
        assert_ne!(error[0].code, "TWLAYOUT-E014");
    }

    #[test]
    fn panorama_zone_overflow_is_diagnosed_without_bridge_spill() {
        let surface =
            resolve_surface_profile(2400, 1080, SurfaceProfileId::ThermalrightCurved2400x1080)
                .expect("curved fixture");
        let error = solve(
            &curved_document(
                vec![
                    metric("one"),
                    metric("two"),
                    metric("three"),
                    metric("four"),
                    metric("five"),
                ],
                None,
            ),
            surface,
        )
        .expect_err("three modules in one local zone must overflow");
        assert_eq!(error[0].code, validation::ZONE_OVERFLOW_CODE);
        assert!(error[0].reason.contains("left-readable"));
    }

    #[test]
    fn square_column_coordinates_use_fixed_cards_and_space_between() {
        let surface = rectangular_surface_profile(480, 480).expect("square fixture");
        let layout = solve(&document(&["first", "last"], None), surface).expect("solve");

        assert_eq!(layout.recipe, RecipeKind::Column);
        assert_eq!(layout.bounds, Rect::new(16, 16, 448, 448));
        assert_eq!(
            layout
                .modules
                .iter()
                .map(|module| (module.id.as_str(), module.bounds))
                .collect::<Vec<_>>(),
            vec![
                ("first", Rect::new(16, 16, 448, 172)),
                ("last", Rect::new(16, 292, 448, 172)),
            ]
        );
        assert_eq!(
            layout.modules[0].bounds.bottom(),
            layout.bounds.bottom() - 172 - 104
        );
        assert_eq!(layout.modules[1].bounds.bottom(), layout.bounds.bottom());
    }

    #[test]
    fn portrait_column_preserves_order_and_rounds_distributed_gaps() {
        let surface = rectangular_surface_profile(480, 1280).expect("portrait fixture");
        let layout = solve(
            &document(&["one", "two", "three", "four", "five", "six"], None),
            surface,
        )
        .expect("solve");

        assert_eq!(layout.recipe, RecipeKind::Column);
        assert_eq!(
            layout
                .modules
                .iter()
                .map(|module| (module.id.as_str(), module.bounds.y))
                .collect::<Vec<_>>(),
            vec![
                ("one", 16),
                ("two", 231),
                ("three", 446),
                ("four", 662),
                ("five", 877),
                ("six", 1092),
            ]
        );
        assert_eq!(
            layout.modules.last().expect("last module").bounds.bottom(),
            1264
        );
    }

    #[test]
    fn wide_two_column_coordinates_keep_two_fixed_tracks() {
        let surface = rectangular_surface_profile(1280, 480).expect("wide fixture");
        let layout = solve(
            &document(&["first", "second", "third", "fourth"], None),
            surface,
        )
        .expect("solve");

        assert_eq!(layout.recipe, RecipeKind::TwoColumn);
        assert_eq!(
            layout
                .modules
                .iter()
                .map(|module| (module.id.as_str(), module.bounds))
                .collect::<Vec<_>>(),
            vec![
                ("first", Rect::new(16, 16, 448, 172)),
                ("second", Rect::new(816, 16, 448, 172)),
                ("third", Rect::new(16, 292, 448, 172)),
                ("fourth", Rect::new(816, 292, 448, 172)),
            ]
        );
        for pair in layout.modules.windows(2) {
            assert!(!pair[0].bounds.overlaps(pair[1].bounds));
        }
    }

    #[test]
    fn repeated_solves_are_byte_equivalent() {
        let surface = rectangular_surface_profile(1280, 480).expect("wide fixture");
        let document = document(&["a", "b", "c"], None);
        let first = serde_json::to_vec(&solve(&document, surface).expect("solve")).expect("json");
        let second = serde_json::to_vec(&solve(&document, surface).expect("solve")).expect("json");
        assert_eq!(first, second);
    }

    #[test]
    fn unsupported_panorama_is_a_stable_diagnostic() {
        let surface = rectangular_surface_profile(480, 480).expect("square fixture");
        let error = solve(
            &document(&["module"], Some(("square", "zoned-panorama"))),
            surface,
        )
        .expect_err("curved recipe is not solved here");

        assert_eq!(error.len(), 1);
        assert_eq!(error[0].code, validation::UNSUPPORTED_RECIPE_CODE);
        assert_eq!(
            error[0].reason,
            "Recipe `zoned-panorama` is not supported in this task."
        );
    }

    #[test]
    fn overflow_is_rejected_before_any_geometry_is_returned() {
        let surface = rectangular_surface_profile(480, 480).expect("square fixture");
        let error = solve(&document(&["a", "b", "c"], None), surface)
            .expect_err("three fixed cards do not fit in square content");

        assert_eq!(error.len(), 1);
        assert_eq!(error[0].code, validation::RECIPE_OVERFLOW_CODE);
        assert!(error[0].reason.contains("capacity 2"));
    }
}
