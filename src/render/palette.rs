//! Overlay color-scheme suggestion from a background image.
//!
//! Pipeline (Material You-shaped): subsample the background to ≤~16k pixels,
//! quantize with Wu + weighted-k-means (`QuantizerCelebi`), rank hues with
//! Material's `Score` (chroma/population weighting + hue dedup), then rebuild
//! each overlay role in HCT taking only the *hue* from the image — lightness
//! (tone) and chroma come from a fixed recipe so accents stay vivid on the
//! backlit LCD regardless of how dark or muddy the source colors are.
//!
//! Contrast guarantee: HCT tone is CIELAB L*, and a tone gap of 45+ over the
//! background guarantees ≳4.5:1 WCAG contrast. The gap is measured against
//! the image's MEDIAN tone — the typical ground the HUD sits on — not its
//! brightest feature: a dark wallpaper with one large bright element (a moon,
//! a neon sun) must not push every accent into washed-out pastel. Accent
//! tones are floored at `max(recipe, bg_median_tone + TONE_GAP)`, capped at 95.
//!
//! Achromatic images (every cluster below Score's chroma cutoff) fall back to
//! the Tokyo Night blue seed instead of inventing hue from sensor noise.

use material_colors::color::Argb;
use material_colors::hct::Hct;
use material_colors::quantize::{Quantizer, QuantizerCelebi};
use material_colors::score::Score;
use material_colors::utils::math::difference_degrees;
use tiny_skia::Pixmap;

/// Fallback seed when the background has no usable chroma (Tokyo Night blue).
const FALLBACK_SEED: Argb = Argb {
    alpha: 255,
    red: 0x7a,
    green: 0xa2,
    blue: 0xf7,
};

/// Max samples fed to the quantizer (Material uses a 128x128 bitmap).
const MAX_SAMPLES: usize = 16_384;
/// Quantizer cluster count (Material production value).
const MAX_COLORS: usize = 128;
/// Min hue separation between distinct image-derived accents.
const MIN_HUE_SEPARATION: f64 = 35.0;
/// Analogous-adjacent rotation for derived accents (Material's tertiary rule).
const DERIVED_ROTATION: f64 = 60.0;
/// Accent tone must exceed the background's median tone by this much (≈4.5:1).
const TONE_GAP: f64 = 45.0;
/// Tones above this are unreachable for chromatic accents; also the raise cap.
const MAX_ACCENT_TONE: f64 = 95.0;

/// One suggested color per overlay role, as `#rrggbb` hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemeSuggestion {
    /// Main accent: gauge arcs, hero numerals (CPU-ish roles).
    pub primary: String,
    /// Second accent: graph lines, second gauge (GPU-ish roles).
    pub secondary: String,
    /// Third accent: bottom stats, FPS highlights.
    pub tertiary: String,
    /// Value text: near-white, faintly tinted toward the primary hue.
    pub text: String,
    /// Labels / muted captions.
    pub dim: String,
    /// Panel base tint: dark, faintly tinted (NOT tone-raised).
    pub panel_bg: String,
}

/// Fixed tone/chroma recipe per role; hue is the only image-driven input.
/// Tones sit at or above the ≈#999999 wash-out floor (L* 65) by construction.
const RECIPES: [(Role, f64, f64); 6] = [
    (Role::Primary, 64.0, 76.0),
    (Role::Secondary, 48.0, 71.0),
    (Role::Tertiary, 72.0, 74.0),
    (Role::Text, 8.0, 92.0),
    (Role::Dim, 16.0, 67.0),
    (Role::PanelBg, 12.0, 21.0),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Primary,
    Secondary,
    Tertiary,
    Text,
    Dim,
    PanelBg,
}

/// Suggest an overlay color scheme for the given rendered background.
///
/// The pixmap should be a small aspect-preserving resize of the full image
/// (see [`crate::render::background::BackgroundImage::to_pixmap`]); premultiplied
/// alpha is demultiplied per pixel, and fully transparent pixels are skipped.
pub fn suggest_scheme(pixmap: &Pixmap) -> SchemeSuggestion {
    let samples = sample_pixels(pixmap);
    let (ranked, bg_median_tone) = rank_hues(&samples);

    let primary_hue = ranked
        .first()
        .map(|hct| hct.get_hue())
        .unwrap_or_else(|| Hct::new(FALLBACK_SEED).get_hue());
    let secondary_hue = pick_distinct_hue(&ranked[1.min(ranked.len())..], &[primary_hue])
        .unwrap_or(primary_hue + DERIVED_ROTATION);
    let tertiary_hue = pick_distinct_hue(
        &ranked[1.min(ranked.len())..],
        &[primary_hue, secondary_hue],
    )
    .unwrap_or(primary_hue + 2.0 * DERIVED_ROTATION);

    // Bright backgrounds push accent tones up to preserve the contrast gap.
    let tone_floor = (bg_median_tone + TONE_GAP).min(MAX_ACCENT_TONE);

    let build = |role: Role| -> String {
        let (_, chroma, base_tone) = RECIPES
            .iter()
            .copied()
            .find(|(r, _, _)| *r == role)
            .expect("recipe defined for every role");
        let hue = match role {
            Role::Primary | Role::Text | Role::Dim | Role::PanelBg => primary_hue,
            Role::Secondary => secondary_hue,
            Role::Tertiary => tertiary_hue,
        };
        // The panel base must stay dark under the overlay; it is the one role
        // exempt from the accent tone floor.
        let tone = if role == Role::PanelBg {
            base_tone
        } else {
            base_tone.max(tone_floor)
        };
        // Hct::from gamut-maps: chroma is reduced to the max displayable at
        // this hue/tone, so the result is always valid sRGB.
        Argb::from(Hct::from(hue, chroma, tone)).to_hex_with_pound()
    };

    SchemeSuggestion {
        primary: build(Role::Primary),
        secondary: build(Role::Secondary),
        tertiary: build(Role::Tertiary),
        text: build(Role::Text),
        dim: build(Role::Dim),
        panel_bg: build(Role::PanelBg),
    }
}

/// Demultiply and subsample the pixmap down to at most `MAX_SAMPLES` pixels.
fn sample_pixels(pixmap: &Pixmap) -> Vec<Argb> {
    let pixels = pixmap.pixels();
    let stride = (pixels.len() / MAX_SAMPLES).max(1);
    pixels
        .iter()
        .step_by(stride)
        .filter(|p| p.alpha() > 0)
        .map(|p| {
            let c = p.demultiply();
            Argb::new(255, c.red(), c.green(), c.blue())
        })
        .collect()
}

/// Quantize + score the samples; also return the median tone (L*) of the
/// sampled pixels, which drives the contrast floor.
fn rank_hues(samples: &[Argb]) -> (Vec<Hct>, f64) {
    if samples.is_empty() {
        return (vec![Hct::new(FALLBACK_SEED)], 0.0);
    }

    let mut tones: Vec<f64> = samples.iter().map(lstar).collect();
    tones.sort_by(|a, b| a.total_cmp(b));
    let median = tones[tones.len() / 2];

    let quantized = QuantizerCelebi::quantize(samples, MAX_COLORS);
    let ranked = Score::score(
        &quantized.color_to_count,
        Some(4),
        Some(FALLBACK_SEED),
        Some(true),
    );
    (ranked.into_iter().map(Hct::new).collect(), median)
}

/// First candidate whose hue is ≥ `MIN_HUE_SEPARATION`° from every taken hue.
fn pick_distinct_hue(candidates: &[Hct], taken: &[f64]) -> Option<f64> {
    candidates.iter().map(|hct| hct.get_hue()).find(|hue| {
        taken
            .iter()
            .all(|t| difference_degrees(*hue, *t) >= MIN_HUE_SEPARATION)
    })
}

/// CIELAB L* (== HCT tone) of an sRGB color, without a full CAM16 conversion.
fn lstar(argb: &Argb) -> f64 {
    fn linearize(channel: u8) -> f64 {
        let c = f64::from(channel) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    let y = 0.212_6 * linearize(argb.red)
        + 0.715_2 * linearize(argb.green)
        + 0.072_2 * linearize(argb.blue);
    if y <= 216.0 / 24_389.0 {
        y * 24_389.0 / 27.0
    } else {
        116.0 * y.cbrt() - 16.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::PremultipliedColorU8;

    fn pixmap_of(colors: &[(u8, u8, u8, usize)]) -> Pixmap {
        let total: usize = colors.iter().map(|(_, _, _, n)| n).sum();
        let side = (total as f64).sqrt().ceil() as u32;
        let mut pixmap = Pixmap::new(side, side).unwrap();
        let mut fill = Vec::with_capacity((side * side) as usize);
        for &(r, g, b, n) in colors {
            fill.extend(std::iter::repeat_n(
                PremultipliedColorU8::from_rgba(r, g, b, 255).unwrap(),
                n,
            ));
        }
        fill.resize(
            (side * side) as usize,
            PremultipliedColorU8::from_rgba(0, 0, 0, 255).unwrap(),
        );
        pixmap.pixels_mut().copy_from_slice(&fill);
        pixmap
    }

    fn hue_of_hex(hex: &str) -> f64 {
        let r = u8::from_str_radix(&hex[1..3], 16).unwrap();
        let g = u8::from_str_radix(&hex[3..5], 16).unwrap();
        let b = u8::from_str_radix(&hex[5..7], 16).unwrap();
        Hct::new(Argb::new(255, r, g, b)).get_hue()
    }

    fn tone_of_hex(hex: &str) -> f64 {
        let r = u8::from_str_radix(&hex[1..3], 16).unwrap();
        let g = u8::from_str_radix(&hex[3..5], 16).unwrap();
        let b = u8::from_str_radix(&hex[5..7], 16).unwrap();
        Hct::new(Argb::new(255, r, g, b)).get_tone()
    }

    #[test]
    fn dominant_vivid_hue_becomes_primary() {
        // Mostly dark pixels with a vivid blue accent region: the blue must
        // win scoring even though dark pixels dominate by count.
        let pixmap = pixmap_of(&[(10, 10, 16, 3000), (58, 96, 244, 1000)]);
        let scheme = suggest_scheme(&pixmap);
        let blue_hue = Hct::new(Argb::new(255, 58, 96, 244)).get_hue();
        assert!(
            difference_degrees(hue_of_hex(&scheme.primary), blue_hue) < 20.0,
            "primary {} should carry the blue hue",
            scheme.primary
        );
    }

    #[test]
    fn accents_meet_the_lcd_tone_floor() {
        let pixmap = pixmap_of(&[(16, 16, 24, 2000), (200, 40, 90, 800)]);
        let scheme = suggest_scheme(&pixmap);
        for hex in [
            &scheme.primary,
            &scheme.secondary,
            &scheme.tertiary,
            &scheme.text,
            &scheme.dim,
        ] {
            assert!(
                tone_of_hex(hex) >= 65.0,
                "{hex} is below the wash-out floor"
            );
        }
        assert!(
            tone_of_hex(&scheme.panel_bg) <= 30.0,
            "panel bg {} must stay dark",
            scheme.panel_bg
        );
    }

    #[test]
    fn accent_hues_are_mutually_distinct() {
        let pixmap = pixmap_of(&[(20, 20, 30, 2000), (90, 200, 255, 700), (250, 120, 60, 700)]);
        let scheme = suggest_scheme(&pixmap);
        let hues = [
            hue_of_hex(&scheme.primary),
            hue_of_hex(&scheme.secondary),
            hue_of_hex(&scheme.tertiary),
        ];
        for i in 0..hues.len() {
            for j in (i + 1)..hues.len() {
                assert!(
                    difference_degrees(hues[i], hues[j]) >= 25.0,
                    "accent hues {i} and {j} are too close: {hues:?}"
                );
            }
        }
    }

    #[test]
    fn achromatic_image_falls_back_to_tokyo_blue() {
        let pixmap = pixmap_of(&[(40, 40, 40, 2000), (90, 90, 90, 2000)]);
        let scheme = suggest_scheme(&pixmap);
        let fallback_hue = Hct::new(FALLBACK_SEED).get_hue();
        assert!(
            difference_degrees(hue_of_hex(&scheme.primary), fallback_hue) < 10.0,
            "primary {} should use the fallback blue hue",
            scheme.primary
        );
    }

    #[test]
    fn monochrome_image_derives_rotated_secondary() {
        // All-cyan image: secondary must come from the +60° rotation, not a
        // duplicate cyan.
        let pixmap = pixmap_of(&[(8, 12, 20, 2000), (60, 200, 230, 1500)]);
        let scheme = suggest_scheme(&pixmap);
        let sep = difference_degrees(hue_of_hex(&scheme.primary), hue_of_hex(&scheme.secondary));
        assert!(
            sep >= 25.0,
            "secondary should be rotated away from primary (sep {sep})"
        );
    }

    #[test]
    fn bright_background_raises_accent_tones() {
        // A bright image (p95 tone high) must push accents up to keep the gap.
        let pixmap = pixmap_of(&[(200, 205, 220, 3000), (120, 150, 255, 1000)]);
        let scheme = suggest_scheme(&pixmap);
        assert!(
            tone_of_hex(&scheme.primary) >= 85.0,
            "primary {} should be raised over a bright background",
            scheme.primary
        );
    }

    #[test]
    fn bright_feature_on_dark_ground_keeps_vivid_accents() {
        // Regression: a dark wallpaper dominated by one large bright element
        // (ronin-moon's pink moon) must NOT push accents into near-white
        // pastel — the floor tracks the median (dark) ground, not the p95.
        let pixmap = pixmap_of(&[(18, 16, 40, 3000), (255, 80, 200, 1000)]);
        let scheme = suggest_scheme(&pixmap);
        let primary = Hct::new({
            let hex = &scheme.primary;
            let r = u8::from_str_radix(&hex[1..3], 16).unwrap();
            let g = u8::from_str_radix(&hex[3..5], 16).unwrap();
            let b = u8::from_str_radix(&hex[5..7], 16).unwrap();
            Argb::new(255, r, g, b)
        });
        assert!(
            primary.get_tone() <= 80.0,
            "primary {} washed out to pastel (tone {})",
            scheme.primary,
            primary.get_tone()
        );
        assert!(
            primary.get_chroma() >= 25.0,
            "primary {} lost its vividness (chroma {})",
            scheme.primary,
            primary.get_chroma()
        );
    }

    #[test]
    fn deterministic_for_same_input() {
        let pixmap = pixmap_of(&[(10, 10, 16, 3000), (58, 96, 244, 1000)]);
        assert_eq!(suggest_scheme(&pixmap), suggest_scheme(&pixmap));
    }
}
