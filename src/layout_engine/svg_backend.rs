//! Deterministic SVG/resvg backend for solved layout scenes.
//!
//! [`Scene`] remains renderer-neutral. This module owns the small amount of
//! serialization needed to turn typed scene nodes into an internal SVG string,
//! then hands that string to `resvg` for CPU rasterization.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use image::GenericImageView;
use image::ImageEncoder;
use resvg::usvg;
use tiny_skia::{Pixmap, Transform};

use super::diagnostic::{DiagnosticSeverity, LayoutDiagnostic};
use super::scene::{
    Color, ImageFit, ImageNode, PathNode, Rect, RectNode, Scene, SceneNode, TextAlignment, TextNode,
};

/// Stable diagnostic code for failures while compiling or rasterizing a scene.
pub const SVG_BACKEND_DIAGNOSTIC_CODE: &str = "TWLAYOUT-E030";

const EMBEDDED_FONT: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
const EMBEDDED_FONT_FAMILY: &str = "DejaVu Sans Mono";

/// A decoded or encoded image available to an [`ImageNode`].
///
/// Image sources are resolved before rendering. The scene only contains an
/// opaque source id, so a document cannot smuggle a file path or SVG fragment
/// into the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaAsset {
    /// Straight RGBA8 pixels with their source dimensions.
    Rgba8 {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    /// Encoded image bytes, such as PNG or JPEG. The MIME type is used only in
    /// the internal data URI generated for SVG.
    Encoded { mime_type: String, bytes: Vec<u8> },
}

impl MediaAsset {
    /// Construct a decoded RGBA8 asset.
    pub fn rgba8(width: u32, height: u32, pixels: impl Into<Vec<u8>>) -> Self {
        Self::Rgba8 {
            width,
            height,
            pixels: pixels.into(),
        }
    }

    /// Construct an encoded asset with an explicit MIME type.
    pub fn encoded(mime_type: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self::Encoded {
            mime_type: mime_type.into(),
            bytes: bytes.into(),
        }
    }
}

/// Runtime image catalog passed to a scene backend.
///
/// The catalog deliberately is not part of the persisted layout document. It
/// is resolved by the caller at the render boundary and is keyed by the
/// logical `ImageNode::source` id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedMedia {
    images: BTreeMap<String, MediaAsset>,
}

impl ResolvedMedia {
    /// Create an empty runtime image catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace an arbitrary runtime image asset.
    pub fn insert(&mut self, id: impl Into<String>, asset: MediaAsset) -> Option<MediaAsset> {
        self.images.insert(id.into(), asset)
    }

    /// Insert or replace decoded RGBA8 pixels.
    pub fn insert_rgba(
        &mut self,
        id: impl Into<String>,
        width: u32,
        height: u32,
        pixels: impl Into<Vec<u8>>,
    ) -> Option<MediaAsset> {
        self.insert(id, MediaAsset::rgba8(width, height, pixels))
    }

    /// Builder form of [`Self::insert_rgba`].
    pub fn with_rgba(
        mut self,
        id: impl Into<String>,
        width: u32,
        height: u32,
        pixels: impl Into<Vec<u8>>,
    ) -> Self {
        self.insert_rgba(id, width, height, pixels);
        self
    }

    /// Insert or replace encoded image bytes, inferring a MIME type from the
    /// image signature when possible.
    pub fn insert_encoded(
        &mut self,
        id: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Option<MediaAsset> {
        let bytes = bytes.into();
        let mime_type = mime_type_for_bytes(&bytes).to_owned();
        self.insert(id, MediaAsset::encoded(mime_type, bytes))
    }

    /// Insert or replace encoded image bytes with an explicit MIME type.
    pub fn insert_encoded_with_mime(
        &mut self,
        id: impl Into<String>,
        mime_type: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Option<MediaAsset> {
        self.insert(id, MediaAsset::encoded(mime_type, bytes))
    }

    /// Builder form of [`Self::insert_encoded`].
    pub fn with_encoded(mut self, id: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        self.insert_encoded(id, bytes);
        self
    }

    /// Builder form of [`Self::insert_encoded_with_mime`].
    pub fn with_encoded_with_mime(
        mut self,
        id: impl Into<String>,
        mime_type: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Self {
        self.insert_encoded_with_mime(id, mime_type, bytes);
        self
    }

    /// Look up an image by its logical scene source id.
    pub fn get(&self, id: &str) -> Option<&MediaAsset> {
        self.images.get(id)
    }

    /// Return whether no runtime images have been resolved.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

/// Backend boundary for a backend-neutral solved scene.
pub trait SceneBackend {
    /// Render at the scene's native dimensions without responsive scaling.
    fn render(&self, scene: &Scene, media: &ResolvedMedia) -> Result<Pixmap, LayoutDiagnostic>;
}

/// Internal SVG compiler and `resvg` CPU rasterizer.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResvgSceneBackend;

impl SceneBackend for ResvgSceneBackend {
    fn render(&self, scene: &Scene, media: &ResolvedMedia) -> Result<Pixmap, LayoutDiagnostic> {
        let svg = compile_scene_xml(scene, media)?;
        let tree = usvg::Tree::from_str(&svg, &usvg_options()).map_err(|error| {
            backend_diagnostic(
                "Failed to parse compiled scene SVG",
                format!("resvg rejected the internal SVG: {error}"),
                "Keep scene values finite and use a supported resolved media asset",
            )
        })?;
        let mut pixmap = Pixmap::new(scene.width, scene.height).ok_or_else(|| {
            backend_diagnostic(
                "Failed to allocate scene pixmap",
                format!(
                    "scene dimensions {}x{} are not renderable",
                    scene.width, scene.height
                ),
                "Use non-zero native target dimensions",
            )
        })?;

        // The root SVG viewBox is exactly the scene canvas, so identity is the
        // only transform needed. In particular, do not apply contain/letterbox
        // scaling here: surface profiles own their native dimensions.
        resvg::render(&tree, Transform::identity(), &mut pixmap.as_mut());
        Ok(pixmap)
    }
}

/// Compile a typed scene into the internal SVG consumed by `resvg`.
///
/// This is public for deterministic preview tooling and backend tests; callers
/// should treat the returned XML as an implementation detail rather than an
/// authoring surface.
pub fn compile_scene_xml(scene: &Scene, media: &ResolvedMedia) -> Result<String, LayoutDiagnostic> {
    if scene.width == 0 || scene.height == 0 {
        return Err(backend_diagnostic(
            "Invalid scene dimensions",
            format!(
                "scene dimensions {}x{} must be non-zero",
                scene.width, scene.height
            ),
            "Choose a target profile with positive native dimensions",
        ));
    }

    let mut xml = String::with_capacity(256 + scene.nodes.len() * 96);
    write!(
        xml,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="0 0 {} {}" overflow="hidden">"#,
        scene.width, scene.height, scene.width, scene.height
    )
    .expect("writing to a String cannot fail");

    let clips = scene
        .nodes
        .iter()
        .filter_map(|node| match node {
            SceneNode::Clip(clip) => Some(clip.bounds),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !clips.is_empty() {
        xml.push_str("<defs>");
        for (index, bounds) in clips.iter().enumerate() {
            write_rect_clip_path(&mut xml, index, *bounds)?;
        }
        xml.push_str("</defs>");
    }

    let mut active_clip = None;
    let mut clip_index = 0;
    for (node_index, node) in scene.nodes.iter().enumerate() {
        if let SceneNode::Clip(clip) = node {
            if active_clip.is_some() {
                xml.push_str("</g>");
            }
            write!(xml, r#"<g clip-path="url(#clip-{})">"#, clip_index)
                .expect("writing to a String cannot fail");
            active_clip = Some(clip.bounds);
            clip_index += 1;
            continue;
        }
        append_node(&mut xml, node, media, node_index)?;
    }
    if active_clip.is_some() {
        xml.push_str("</g>");
    }
    xml.push_str("</svg>");
    Ok(xml)
}

fn append_node(
    xml: &mut String,
    node: &SceneNode,
    media: &ResolvedMedia,
    node_index: usize,
) -> Result<(), LayoutDiagnostic> {
    match node {
        SceneNode::Rect(rect) => append_rect(xml, rect),
        SceneNode::Path(path) => append_path(xml, path),
        SceneNode::Text(text) => append_text(xml, text),
        SceneNode::Image(image) => append_image(xml, image, media, node_index),
        SceneNode::Clip(_) => Ok(()),
    }
}

fn append_rect(xml: &mut String, node: &RectNode) -> Result<(), LayoutDiagnostic> {
    let opacity = format_float(node.opacity, "rectangle opacity")?;
    write!(
        xml,
        r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}" opacity="{}"/>"#,
        node.bounds.x,
        node.bounds.y,
        node.bounds.width,
        node.bounds.height,
        node.fill.to_hex(),
        opacity
    )
    .expect("writing to a String cannot fail");
    Ok(())
}

fn append_path(xml: &mut String, node: &PathNode) -> Result<(), LayoutDiagnostic> {
    let opacity = format_float(node.opacity, "path opacity")?;
    let stroke_width = format_float(node.stroke_width, "path stroke width")?;
    if node.stroke_width < 0.0 {
        return Err(backend_diagnostic(
            "Invalid path stroke width",
            "path stroke width cannot be negative",
            "Use a finite non-negative stroke width",
        ));
    }
    if node.points.is_empty() {
        return Ok(());
    }

    let mut d = String::new();
    for (index, point) in node.points.iter().enumerate() {
        let x = format_float(point.x, "path x coordinate")?;
        let y = format_float(point.y, "path y coordinate")?;
        if index == 0 {
            write!(d, "M {x} {y}").expect("writing to a String cannot fail");
        } else {
            write!(d, " L {x} {y}").expect("writing to a String cannot fail");
        }
    }
    if node.closed {
        d.push_str(" Z");
    }
    let fill = node
        .fill
        .map(Color::to_hex)
        .unwrap_or_else(|| "none".to_owned());
    write!(
        xml,
        r#"<path d="{}" fill="{}" stroke="{}" stroke-width="{}" stroke-linecap="round" stroke-linejoin="round" opacity="{}"/>"#,
        escape_xml(&d),
        fill,
        node.stroke.to_hex(),
        stroke_width,
        opacity
    )
    .expect("writing to a String cannot fail");
    Ok(())
}

fn append_text(xml: &mut String, node: &TextNode) -> Result<(), LayoutDiagnostic> {
    let opacity = format_float(node.opacity, "text opacity")?;
    if node.font_size == 0 {
        return Err(backend_diagnostic(
            "Invalid text font size",
            "text font size must be positive",
            "Use a positive semantic text size",
        ));
    }
    let (x, anchor) = match node.alignment {
        TextAlignment::Start => (node.bounds.x as f32, "start"),
        TextAlignment::Center => (
            node.bounds.x as f32 + node.bounds.width as f32 / 2.0,
            "middle",
        ),
        TextAlignment::End => (node.bounds.right() as f32, "end"),
    };
    let x = format_float(x, "text x coordinate")?;
    let y = format_float(
        node.bounds.y as f32 + node.bounds.height as f32 / 2.0,
        "text y coordinate",
    )?;
    write!(
        xml,
        r#"<text x="{}" y="{}" text-anchor="{}" dominant-baseline="middle" font-family="{}" font-size="{}" fill="{}" opacity="{}">{}</text>"#,
        x,
        y,
        anchor,
        escape_xml(EMBEDDED_FONT_FAMILY),
        node.font_size,
        node.color.to_hex(),
        opacity,
        escape_xml(&node.content),
    )
    .expect("writing to a String cannot fail");
    Ok(())
}

fn append_image(
    xml: &mut String,
    node: &ImageNode,
    media: &ResolvedMedia,
    node_index: usize,
) -> Result<(), LayoutDiagnostic> {
    let (_, _, mime_type, bytes) = media_data(media, &node.source)?;
    let opacity = format_float(node.opacity, "image opacity")?;
    let preserve_aspect_ratio = match node.fit {
        ImageFit::Contain => "xMidYMid meet",
        ImageFit::Cover => "xMidYMid slice",
    };
    let data_uri = format!(
        "data:{};base64,{}",
        mime_type,
        base64::engine::general_purpose::STANDARD.encode(bytes),
    );
    let clip_id = format!("media-{node_index}");
    write!(
        xml,
        r#"<defs><clipPath id="{}"><rect x="{}" y="{}" width="{}" height="{}"/></clipPath></defs><image x="{}" y="{}" width="{}" height="{}" href="{}" preserveAspectRatio="{}" opacity="{}" clip-path="url(#{})"/>"#,
        clip_id,
        node.bounds.x,
        node.bounds.y,
        node.bounds.width,
        node.bounds.height,
        node.bounds.x,
        node.bounds.y,
        node.bounds.width,
        node.bounds.height,
        escape_xml(&data_uri),
        preserve_aspect_ratio,
        opacity,
        clip_id,
    )
    .expect("writing to a String cannot fail");
    Ok(())
}

fn write_rect_clip_path(
    xml: &mut String,
    index: usize,
    bounds: Rect,
) -> Result<(), LayoutDiagnostic> {
    write!(
        xml,
        r#"<clipPath id="clip-{}"><rect x="{}" y="{}" width="{}" height="{}"/></clipPath>"#,
        index, bounds.x, bounds.y, bounds.width, bounds.height
    )
    .expect("writing to a String cannot fail");
    Ok(())
}

fn media_data(
    media: &ResolvedMedia,
    source: &str,
) -> Result<(u32, u32, String, Vec<u8>), LayoutDiagnostic> {
    let asset = media.get(source).ok_or_else(|| {
        backend_diagnostic(
            "Missing resolved media asset",
            format!("image source `{source}` was not found in the resolved media catalog"),
            "Resolve every ImageNode source before rendering the scene",
        )
    })?;

    match asset {
        MediaAsset::Rgba8 {
            width,
            height,
            pixels,
        } => {
            if *width == 0 || *height == 0 {
                return Err(backend_diagnostic(
                    "Invalid resolved media dimensions",
                    "decoded RGBA media must have non-zero dimensions",
                    "Provide a non-empty decoded image",
                ));
            }
            let expected = usize::try_from(*width)
                .ok()
                .and_then(|width| {
                    usize::try_from(*height)
                        .ok()
                        .and_then(|height| width.checked_mul(height))
                })
                .and_then(|pixels| pixels.checked_mul(4));
            if expected != Some(pixels.len()) {
                return Err(backend_diagnostic(
                    "Invalid resolved media pixels",
                    format!(
                        "decoded RGBA media `{source}` has {} bytes but its dimensions require {}",
                        pixels.len(),
                        expected.unwrap_or(usize::MAX)
                    ),
                    "Provide exactly width × height × 4 RGBA bytes",
                ));
            }
            let png = encode_rgba_png(*width, *height, pixels)?;
            Ok((*width, *height, "image/png".to_owned(), png))
        }
        MediaAsset::Encoded { mime_type, bytes } => {
            let decoded = image::load_from_memory(bytes).map_err(|error| {
                backend_diagnostic(
                    "Invalid resolved media bytes",
                    format!("could not decode image source `{source}`: {error}"),
                    "Provide PNG, JPEG, or another image format supported by the image crate",
                )
            })?;
            let (width, height) = decoded.dimensions();
            if width == 0 || height == 0 {
                return Err(backend_diagnostic(
                    "Invalid resolved media dimensions",
                    format!("encoded image source `{source}` has zero dimensions"),
                    "Provide a non-empty encoded image",
                ));
            }
            let mime_type = if mime_type.trim().is_empty() {
                mime_type_for_bytes(bytes).to_owned()
            } else {
                mime_type.clone()
            };
            Ok((width, height, mime_type, bytes.clone()))
        }
    }
}

fn encode_rgba_png(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, LayoutDiagnostic> {
    let mut encoded = Vec::new();
    image::codecs::png::PngEncoder::new(&mut encoded)
        .write_image(pixels, width, height, image::ColorType::Rgba8.into())
        .map_err(|error| {
            backend_diagnostic(
                "Failed to encode resolved media",
                format!("could not encode decoded RGBA media as PNG: {error}"),
                "Provide valid RGBA8 pixels for the media asset",
            )
        })?;
    Ok(encoded)
}

fn mime_type_for_bytes(bytes: &[u8]) -> &'static str {
    match image::guess_format(bytes).ok() {
        Some(image::ImageFormat::Png) => "image/png",
        Some(image::ImageFormat::Jpeg) => "image/jpeg",
        Some(image::ImageFormat::Gif) => "image/gif",
        Some(image::ImageFormat::WebP) => "image/webp",
        _ => "application/octet-stream",
    }
}

fn format_float(value: f32, what: &str) -> Result<String, LayoutDiagnostic> {
    if !value.is_finite() {
        return Err(backend_diagnostic(
            "Invalid scene number",
            format!("{what} must be finite"),
            "Use finite geometry, opacity, and stroke values",
        ));
    }
    let value = if value == 0.0 { 0.0 } else { value };
    let mut formatted = format!("{value:.4}");
    while formatted.contains('.') && formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    Ok(formatted)
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn backend_diagnostic(
    message: impl Into<String>,
    reason: impl Into<String>,
    fix: impl Into<String>,
) -> LayoutDiagnostic {
    LayoutDiagnostic::new(
        SVG_BACKEND_DIAGNOSTIC_CODE,
        DiagnosticSeverity::Error,
        message,
        reason,
        fix,
    )
}

static FONTDB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();

fn fontdb() -> Arc<usvg::fontdb::Database> {
    Arc::clone(FONTDB.get_or_init(|| {
        let mut database = usvg::fontdb::Database::new();
        database.load_font_data(EMBEDDED_FONT.to_vec());
        database.set_monospace_family(EMBEDDED_FONT_FAMILY);
        Arc::new(database)
    }))
}

fn usvg_options() -> usvg::Options<'static> {
    usvg::Options {
        font_family: EMBEDDED_FONT_FAMILY.to_owned(),
        fontdb: fontdb(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_engine::scene::{
        ClipNode, Color, ImageNode, PathNode, Point, RectNode, Scene, SceneNode, TextRole,
    };

    fn pixel(pixmap: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * pixmap.width() + x) * 4) as usize;
        pixmap.data()[offset..offset + 4]
            .try_into()
            .expect("pixel is four bytes")
    }

    fn fixture_scene(width: u32, height: u32) -> Scene {
        Scene::with_nodes(
            width,
            height,
            vec![
                SceneNode::Rect(RectNode::new(
                    Rect::new(0, 0, width, height),
                    Color::rgb(0x10, 0x18, 0x24),
                    1.0,
                )),
                SceneNode::Text(TextNode::new(
                    Rect::new(width / 4, height / 4, width / 2, height / 4),
                    "metric & <sparkline>",
                    TextRole::Value,
                    TextAlignment::Center,
                    Color::rgb(0xee, 0xee, 0xee),
                    24,
                    1.0,
                )),
                SceneNode::Clip(ClipNode::new(Rect::new(
                    width / 4,
                    height / 2,
                    width / 2,
                    height / 4,
                ))),
                SceneNode::Path({
                    let mut path = PathNode::new(
                        Rect::new(0, 0, width, height),
                        vec![
                            Point::new(0.0, height as f32),
                            Point::new(width as f32 / 2.0, 0.0),
                            Point::new(width as f32, height as f32),
                        ],
                        Color::rgb(0x55, 0xdd, 0x88),
                        3.0,
                        1.0,
                    );
                    path.fill = Some(Color::rgb(0x22, 0x66, 0x44));
                    path.closed = true;
                    path
                }),
                SceneNode::Image(ImageNode::new(
                    Rect::new(0, 0, width, height),
                    "media/full-span",
                    ImageFit::Cover,
                    0.35,
                )),
            ],
        )
    }

    #[test]
    fn renders_metric_text_sparkline_and_media_fixture_deterministically() {
        let media = ResolvedMedia::new().with_rgba(
            "media/full-span",
            2,
            2,
            [
                0x20, 0x40, 0x80, 0xff, 0x20, 0x40, 0x80, 0xff, 0x20, 0x40, 0x80, 0xff, 0x20, 0x40,
                0x80, 0xff,
            ],
        );
        let backend = ResvgSceneBackend;
        let first = backend
            .render(&fixture_scene(480, 480), &media)
            .expect("fixture scene should render");
        let second = backend
            .render(&fixture_scene(480, 480), &media)
            .expect("fixture scene should render twice");
        assert_eq!((first.width(), first.height()), (480, 480));
        assert_eq!(first.data(), second.data());
    }

    #[test]
    fn output_dimensions_match_native_target_profiles() {
        let backend = ResvgSceneBackend;
        for (width, height) in [(480, 480), (480, 1280), (1280, 480), (2400, 1080)] {
            let pixmap = backend
                .render(&Scene::new(width, height), &ResolvedMedia::default())
                .expect("native profile should render");
            assert_eq!((pixmap.width(), pixmap.height()), (width, height));
        }
    }

    #[test]
    fn simple_rect_fills_are_rasterized_at_native_coordinates() {
        let scene = Scene::with_nodes(
            12,
            8,
            vec![SceneNode::Rect(RectNode::new(
                Rect::new(2, 1, 6, 4),
                Color::rgb(0x44, 0xaa, 0xee),
                1.0,
            ))],
        );
        let pixmap = ResvgSceneBackend
            .render(&scene, &ResolvedMedia::default())
            .expect("simple fill should render");
        assert_eq!(pixel(&pixmap, 4, 2), [0x44, 0xaa, 0xee, 0xff]);
        assert_eq!(pixel(&pixmap, 0, 0)[3], 0);
    }

    #[test]
    fn dynamic_text_is_xml_escaped() {
        let scene = Scene::with_nodes(
            120,
            80,
            vec![SceneNode::Text(TextNode::new(
                Rect::new(0, 0, 120, 80),
                r#"<&>\"' </text><rect x='0' y='0' width='120' height='80' fill='red'>"#,
                TextRole::Body,
                TextAlignment::Start,
                Color::rgb(0xee, 0xee, 0xee),
                16,
                1.0,
            ))],
        );
        let xml = compile_scene_xml(&scene, &ResolvedMedia::default()).expect("compile");
        assert!(xml.contains("&lt;/text&gt;&lt;rect"));
        assert!(!xml.contains("</text><rect"));
        ResvgSceneBackend
            .render(&scene, &ResolvedMedia::default())
            .expect("escaped text should remain valid SVG");
    }

    #[test]
    fn clip_regions_limit_following_path_pixels() {
        let mut path = PathNode::new(
            Rect::new(0, 0, 32, 32),
            vec![
                Point::new(0.0, 0.0),
                Point::new(32.0, 0.0),
                Point::new(32.0, 32.0),
                Point::new(0.0, 32.0),
            ],
            Color::rgb(0xff, 0x20, 0x20),
            1.0,
            1.0,
        );
        path.fill = Some(Color::rgb(0xff, 0x20, 0x20));
        path.closed = true;
        let scene = Scene::with_nodes(
            32,
            32,
            vec![
                SceneNode::Clip(ClipNode::new(Rect::new(8, 8, 8, 8))),
                SceneNode::Path(path),
            ],
        );
        let pixmap = ResvgSceneBackend
            .render(&scene, &ResolvedMedia::default())
            .expect("clipped path should render");
        assert!(pixel(&pixmap, 10, 10)[3] > 0);
        assert_eq!(pixel(&pixmap, 2, 2)[3], 0);
        assert_eq!(pixel(&pixmap, 24, 24)[3], 0);
    }

    #[test]
    fn full_span_media_covers_native_canvas() {
        let scene = Scene::with_nodes(
            24,
            12,
            vec![SceneNode::Image(ImageNode::new(
                Rect::new(0, 0, 24, 12),
                "wallpaper",
                ImageFit::Cover,
                1.0,
            ))],
        );
        let media = ResolvedMedia::new().with_rgba("wallpaper", 1, 1, [0x28, 0x70, 0xd0, 0xff]);
        let pixmap = ResvgSceneBackend
            .render(&scene, &media)
            .expect("full-span media should render");
        assert_eq!((pixmap.width(), pixmap.height()), (24, 12));
        assert_eq!(pixel(&pixmap, 0, 0), [0x28, 0x70, 0xd0, 0xff]);
        assert_eq!(pixel(&pixmap, 23, 11), [0x28, 0x70, 0xd0, 0xff]);
    }
}
