# Rendering Engine Reference

## Pipeline Overview

```
HTML template (with {{ sensor_vars }})
    ↓ tera::Tera::one_off() — variable substitution + conditionals
Rendered HTML string
    ↓ parser::parse_html() — recursive descent parser
Element tree (tag, style, text, children)
    ↓ layout::compute_layout() — taffy flexbox engine
Vec<LayoutNode> (absolute x, y, width, height)
    ↓ draw::render_nodes() — tiny-skia pixel rendering + fontdue text
Pixmap (RGBA)
    ↓ encode_jpeg() — image crate JPEG encoding + rotation
JPEG bytes → USB bulk transfer
```

## Source Files

| File | Purpose | Lines |
|------|---------|-------|
| `src/render/mod.rs` | Pipeline orchestration, `TemplateRenderer` | ~66 |
| `src/render/parser.rs` | HTML/CSS subset parser | ~262 |
| `src/render/layout.rs` | Taffy integration, absolute positioning | ~146 |
| `src/render/draw.rs` | Pixel rendering, text blitting | ~131 |

## Tera Template System

Full Tera 1.x syntax is available:

### Variables
```
{{ cpu_temp }}                          — direct substitution
{{ cpu_temp | default(value="--") }}    — fallback for missing keys
{{ cpu_temp | int }}                    — cast string to integer
{{ cpu_util | round(precision=0) }}     — round float
```

### Conditionals
```
{% if cpu_temp and cpu_temp | int > 85 %}
  ...hot styling...
{% elif cpu_temp and cpu_temp | int > 60 %}
  ...warm styling...
{% else %}
  ...cool styling...
{% endif %}
```

**Gotcha:** Sensor values are strings. Use `| int` or `| float` for numeric comparison. Always guard with `cpu_temp and` to handle missing keys.

### Loops and Macros
Available but rarely useful for fixed layouts. Could be used for dynamic metric lists in future.

## HTML Parser Details

### Supported Elements
All tag names are parsed identically — `<div>`, `<span>`, and any other tag name all create the same element type. The distinction is purely semantic for template readability.

### Attributes
Only `style=""` is parsed. All other attributes are ignored.

### Text Content
Text between tags is captured as the element's `text` field. Multi-byte UTF-8 (e.g., °C) is handled correctly. Only leaf elements can have text — if an element has children, text nodes between children are captured but may not render as expected.

### Parser Limitations
- No error recovery — a malformed tag halts parsing
- **No HTML comments** — `<!-- -->` is parsed as a tag with name `!`, corrupting the entire layout tree. This is the #1 parser gotcha.
- No HTML entities (`&amp;`, `&lt;`) — use literal characters
- No self-closing tags (`<br/>`, `<img/>`)
- Attribute values must be quoted (single or double)

## Taffy Layout Engine

### What Works
- Flex container with row/column direction
- justify-content and align-items
- gap between children
- Explicit width/height on any element
- Uniform padding and margin
- Nested flex containers (any depth)

### What Does NOT Work
- `flex-grow`, `flex-shrink`, `flex-basis` — not mapped to taffy styles
- `flex-wrap` — not mapped
- Content-based sizing — taffy has no text measure function, so elements without explicit dimensions get 0 intrinsic size
- Per-side padding/margin — only uniform values
- Percentage units — only pixel values
- `order` property — not mapped

### Critical: Content-Based Sizing is Absent

This is the single most important constraint. In a browser, a `<span>` with text automatically gets sized to fit that text. In thermalwriter, it gets **0×0 pixels** unless you set explicit `width` and `height`.

This means:
1. Every text element needs explicit `height`
2. Every container needs explicit dimensions or will be sized by its children (which may be 0)
3. The root element MUST have `width` and `height` matching the display

## Pixel Rendering (draw.rs)

### Background Drawing
Solid color rectangles via `pixmap.fill_rect()`. No rounded corners, no gradients, no borders.

### Text Rendering
- Font: JetBrains Mono Regular (embedded at compile time, ~343KB)
- Rasterization: fontdue per-character bitmap generation
- Anti-aliasing: automatic from fontdue's bitmap output
- Alpha blending: straight alpha blend against existing pixels
- Vertical centering: text centered within element's `height`
- Horizontal alignment: `text-align` controls starting X position

### Text Rendering Quirks
- Text is NOT clipped to container bounds — long text overflows
- No text wrapping — all text renders on a single line
- No bold/italic — only Regular weight is embedded
- `font-family` is parsed but ignored — always JetBrains Mono
- Monospace means numeric values maintain fixed width as they change

## Extending the Engine

### Adding a New CSS Property

1. **Parser** (`parser.rs`): Add to `ElementStyle` struct and `parse_style()` match
2. **Layout** (`layout.rs`): If layout-affecting, map to taffy style in `to_taffy_style()`
3. **Draw** (`draw.rs`): If visual, implement in `render_nodes()`

Example: implementing `border-radius` (already parsed, needs draw support):
```rust
// In draw.rs render_nodes(), after fill_rect:
if let Some(radius) = node.style.border_radius {
    // Use tiny-skia's clip path with rounded rect
    // Requires creating a ClipPath with rounded corners
}
```

### Adding Image Support

Would require:
1. New element type or style property for image source
2. Image loading (from file path or embedded bytes)
3. Image scaling/fitting to container dimensions
4. Blitting pixels onto the pixmap in `draw.rs`

tiny-skia supports `PixmapPaint` for compositing pixmaps, so the drawing is straightforward. The template syntax and loading pipeline are the main work.

### Adding a New Font Weight

1. Add font file to `assets/fonts/` (e.g., `JetBrainsMono-Bold.ttf`)
2. Embed via `include_bytes!()` in `draw.rs`
3. Add font-weight parsing to `parser.rs`
4. Select font in `draw_text()` based on weight

### Adding a New Sensor Provider

1. Create `src/sensor/my_provider.rs` implementing the `SensorProvider` trait
2. Register in `SensorHub::new()` or via config
3. Provider returns `HashMap<String, String>` of key-value pairs
4. Keys become available as `{{ key }}` in templates

## JPEG Output

- Encoding via `image` crate's JPEG encoder
- Quality: configurable 1-100 (default 85)
- Rotation: 0°, 90°, 180°, 270° applied before encoding (default 180° for Peerless Vision cooler)
- Typical frame size: 15-25KB at quality 85
- USB bulk transfer: ~1ms per frame at these sizes
