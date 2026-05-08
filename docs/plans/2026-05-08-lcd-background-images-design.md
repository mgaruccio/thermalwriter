---
date: 2026-05-08
topic: lcd-background-images
---

# LCD Background Images — Standalone Configuration

## What We're Building

A daemon-level global background image feature, decoupled from metrics layouts. The user picks one background image; the daemon composites it under whatever layout is active. Switching layouts doesn't touch the background.

Today, "background image" only works as inline base64 baked into individual SVG layout files (per existing `feedback_background_images.md`). A 2 MB `neon-dash-v2.svg` is mostly the embedded image. This couples background to layout, makes layout files unwieldy, and means every layout has to ship its own copy. This design separates the two concerns.

## Why This Approach

Brainstormed five framing decisions, picking the simplest at each step:

- **Global, one-at-a-time** over per-layout backgrounds. Per-layout overrides are tracked as a future GH issue but add scope (config schema for per-layout, GUI per-layout selection) without proportionate value for V1.
- **Daemon-side composition** over SVG-wrapping or Tera injection. Decoded background Pixmap is cached once and blitted under each rendered frame — bg image size has zero per-tick cost. Layouts don't have to know about backgrounds. Other approaches force the background through resvg every tick (string copies, repeated decode) and only stay competitive with caching that we'd have to bolt on anyway.
- **Files in `~/.config/thermalwriter/backgrounds/`** over absolute paths in config or base64-in-config. Mirrors the existing `seed_layout_dir` pattern. Enables a thumbnail-gallery GUI without filesystem browsing. Self-contained, easy to back up.
- **File gallery + select only** for V1 GUI. Gemini Nano Banana in-GUI generation and Browse-anywhere file picker are tracked as future GH issues — out of scope for V1 to keep the implementation tight.
- **Update seeded layouts** to remove their full-canvas opaque background rects so the global bg actually shines through. Otherwise the feature ships invisible to existing users.

## Key Decisions

- **Storage:** `~/.config/thermalwriter/backgrounds/`. Config holds the filename only: `[background] image = "skyline.png"` (`Option<String>`; unset/empty = no background). First-run seeds 1-2 default backgrounds via the existing `seed_*` pattern.
- **Composition:** at daemon startup and on background change, decode the bg image (PNG/JPEG via the `image` crate, already a dep) into a `tiny_skia::Pixmap`. Cache it on the tick loop's frame source. Each render: blit cached bg → render layout SVG on top → encode JPEG. Bg decode is one-shot per change, not per tick.
- **Layout-side change:** remove the full-canvas opaque `<rect fill="{{ theme_background }}"/>` from each of the 4 seeded SVG layouts (`neon-dash-v2`, `neon-dash`, `arc-gauge`, `cyber-grid`). Panel-level rects (which use `url(#panelGrad)` or sized smaller than 480×480) stay. Layouts still render correctly standalone via Tera defaults; with a global bg set, the bg becomes visible behind the panels. The user's existing 2 MB `neon-dash-v2.svg` in their config dir is untouched — they can replace it with the updated seeded version when convenient.
- **D-Bus surface:** three new methods on `com.thermalwriter.Display`, mirroring the existing layout-control pattern in `f63ff33`:
  - `SetBackground(name: String) -> ()` — validates path under `backgrounds/`, persists `[background].image`, updates in-memory Config, notifies tick loop to re-decode and swap cached Pixmap.
  - `ClearBackground() -> ()` — clears the field, drops the cached Pixmap.
  - `ListBackgrounds() -> Vec<String>` — directory listing, like `ListLayouts`.
- **Tauri commands:** `list_backgrounds() -> Vec<String>`, `set_background(name: Option<String>) -> ()`, `get_active_background() -> Option<String>`. Mirrors the layout commands in shape and uses `AppError` consistently.
- **Config schema:** new `[background]` section with `image: Option<String>`. The dead `[theme].background_image` field that never had a code path reading it gets deleted in the same pass.
- **GUI:** a second panel beside the layout picker showing a thumbnail grid of `~/.config/thermalwriter/backgrounds/` plus a "None" tile. Click to select. Apply path uses the existing save_config + apply_to_daemon flow extended with the bg field.
- **Path traversal:** bg-name validation uses the same `canonicalize() + starts_with()` helper pattern from `validate_layout_path` (Task 4 of the prior plan). Bg-name canonicalization rejects any path escaping `~/.config/thermalwriter/backgrounds/`.

## External Prerequisites

- None for V1. Gemini Nano Banana API integration is deferred to a future GH issue (the API key is already in your environment per the existing `reference_gemini_api.md` memory; in-GUI generation just isn't part of V1's scope).

## Open Questions

None blocking. Implementation specifics to settle during writing-plans:

- Cache invalidation on bg file mutation (filesystem watch vs. trust-the-D-Bus-call). Lean: trust the D-Bus call; manual edits to a file under `backgrounds/` won't auto-refresh until next selection. Documented behavior, not a bug.
- Default seeded backgrounds — what we ship. Probably 1-2 dark, low-distraction patterns or solids. Decide during planning.
- Whether the GUI's "None" tile is its own thumbnail or a button at the top of the gallery.

## Future Enhancements (file as GH issues alongside this work)

1. Per-layout background overrides (`[layout_vars."<name>"].background = "..."` precedence over global)
2. Gemini Nano Banana in-GUI generation
3. Browse-anywhere file picker that copies the chosen file into the backgrounds dir
4. `bg_opacity` knob (0.0-1.0) to dim the bg for LCD backlight washout
5. Background effects (blur, tint overlay, dim color)

## Next Steps

→ writing-plans skill for implementation details
