---
date: 2026-03-23
topic: rendering-pipeline-upgrade
---

# Rendering Pipeline Upgrade

## What We're Building

Two alternative rendering pipelines, both implementing the existing `FrameSource` trait, to replace the current limited HTML subset renderer:

1. **SVG + resvg** — SVG templates with Tera variable substitution, rendered via resvg (which already uses tiny-skia internally)
2. **Blitz HTML/CSS** — Keep HTML/CSS templates but use Blitz (Rust-native, uses Firefox's Stylo CSS engine) for real CSS rendering

Both are spikes to evaluate which authoring format is better for the product. The `FrameSource` trait is already the right abstraction for pluggable renderers.

## Why Two Approaches

- **SVG** gives us gradients, rounded corners, arcs, gauges, and design tool support (Inkscape/Figma) immediately. Trade-off: absolute positioning instead of flexbox.
- **Blitz** keeps the familiar HTML/CSS authoring model and gets real CSS support. Trade-off: alpha software, may have bugs/missing features.

Building both lets us compare authoring ergonomics and rendering quality on the actual hardware.

## Key Decisions

- **FrameSource trait unchanged**: Both renderers implement the existing trait. No architecture changes needed.
- **Tera templating stays**: Both pipelines use Tera for `{{ sensor_variable }}` substitution before rendering.
- **Existing layouts preserved**: The current TemplateRenderer stays as-is. New renderers are additive.

## Architecture

```
FrameSource (trait)
├── TemplateRenderer (current — custom HTML subset, stays as-is)
├── SvgRenderer (new — Tera + resvg → Pixmap)
├── BlitzRenderer (new — Tera + Blitz → Pixmap)
├── (future: StaticImageSource, GifPlayer, ScreenMirror, LlmStream, Doom, etc.)
```

## Open Questions

- Does Blitz work headless at its current alpha stage?
- How does SVG authoring feel for layouts that are primarily text + numbers?
- Which produces better results on the actual LCD hardware?

## Next Steps

Parallel implementation spikes in isolated worktrees, then compare on hardware.
