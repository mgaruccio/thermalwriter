# LCD Background Images Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use forge:executing-plans to implement this plan task-by-task.

**Goal:** Add a daemon-level global LCD background image feature, decoupled from metrics layouts. User picks one image; daemon caches the decoded pixmap and composites it under whatever layout is active. Switching layouts doesn't touch the background.

**Architecture:** Cargo workspace already split between daemon and Tauri GUI. Backgrounds live as PNG/JPEG files in `~/.config/thermalwriter/backgrounds/` (mirrors the existing `seed_layout_dir` pattern). Config holds just the filename via a new `[background]` section. Daemon decodes the bg image once on startup or on D-Bus-triggered change; `SvgRenderer` composites its rendered output onto the cached background before producing `RawFrame`. New D-Bus methods (`SetBackground`/`ClearBackground`/`ListBackgrounds`) mirror the existing `SetLayout`/`ListLayouts` pattern. GUI gets a thumbnail-gallery panel beside the layout picker.

**Tech Stack:** Rust (`tiny_skia`, `image`, `zbus`, `toml_edit`), Svelte 5 + Tauri 2, no new dependencies (all already in tree).

**Required Skills:**
- `forge:writing-tests`: Invoke before any test writing — TDD discipline, meaningful assertions over shape-only.
- `forge:expectation-driven-development`: Invoke before running verification commands — pre-register expected output, investigate violations.
- `forge:verification-before-completion`: Invoke before claiming any task complete — run the command, read the output, then claim.
- `forge:escalate-not-accommodate`: Invoke when something fails unexpectedly — surface the finding, don't write code that tolerates broken systems.

## Context for Executor

### Key Files

**Daemon side:**
- `src/config.rs:83-114` — `Config` struct. Add `pub background: BackgroundConfig` field with `#[serde(default)]`.
- `src/config.rs:75-81` — `ThemeConfig` has a dead `background_image: Option<String>` field that nothing reads. Delete in this plan to reduce confusion.
- `src/config.rs:269` — `seed_layout_dir` pattern. Mirror as `seed_background_dir` for first-run defaults.
- `src/render/svg.rs:36-91` — `SvgRenderer` impl block. Add `pub fn set_background(&mut self, bg: Option<tiny_skia::Pixmap>)` similar to `set_theme` / `set_history` / `set_layout_vars`.
- `src/render/svg.rs:93-145` — `FrameSource for SvgRenderer<'static>` with `render()` method at line 94. This is where compositing happens: render layout to its own Pixmap, then if `self.background` is `Some`, blit the bg first and draw layout on top.
- `src/render/svg.rs:144` — `RawFrame::from_pixmap(&pixmap)` final step. The composited pixmap goes through this.
- `src/main.rs:139-153` — Initial `SvgRenderer` construction. After `set_layout_vars`, also call `set_background(decoded_bg)`.
- `src/main.rs:186-219` — Mode change handler. **Currently buggy:** constructs new SvgRenderer with `ThemePalette::default()` (line 198) instead of the configured theme, and never calls `set_history()`. Plan fixes this in Task 1.
- `src/service/dbus.rs:19-23` — `ModeChange` enum. Add `Background { image: Option<tiny_skia::Pixmap> }` variant.
- `src/service/dbus.rs:35-50` — `ServiceState` struct. Add `pub current_background: Option<tiny_skia::Pixmap>` and `pub background_dir: PathBuf` fields.
- `src/service/dbus.rs:230-330` — `DisplayInterface` impl. Add `SetBackground`/`ClearBackground`/`ListBackgrounds` D-Bus methods. Mirror the `set_layout_vars` pattern at line ~260 (path validation via `validate_layout_path` style helper, persist via Config save method, send `ModeChange::Background` to tick loop).
- `src/service/dbus.rs:200-220` — `validate_layout_path` helper. Mirror as `validate_background_path` (or generalize into one helper that takes a base dir).

**GUI side:**
- `gui/src-tauri/src/commands.rs:71-238` — Existing Tauri commands. Add `list_backgrounds`, `set_background(name: Option<String>)`, `get_active_background` mirroring shape of `list_layouts`/`save_config` etc. All return `Result<T, AppError>`.
- `gui/src-tauri/src/lib.rs:27-39` — `tauri::generate_handler!` registration. Add the three new commands.
- `gui/src-tauri/src/error.rs` — `AppError` enum. Add `BackgroundDecode(String)` and `BackgroundIo(String)` variants if needed (or fold into existing variants).
- `gui/src/App.svelte` — Top-level component. Extend with a second panel beside the existing layout picker.
- `gui/src/lib/` — New file `BgGallery.svelte` for the thumbnail panel.

**Layout files (Phase 2):**
- `layouts/svg/neon-dash-v2.svg:22` — `<rect width="480" height="480" fill="{{ theme_background }}"/>`. Remove this rect.
- `layouts/svg/neon-dash.svg:29` — `<rect width="480" height="480" rx="0" fill="{{ theme_background }}"/>`. Remove.
- `layouts/svg/arc-gauge.svg:49` — `<rect width="480" height="480" fill="url(#bgGrad)"/>`. Remove. The `bgGrad` linearGradient def at line ~3 can stay (unused defs are harmless) or also be removed — your call during execution.
- `layouts/svg/cyber-grid.svg:48-49` — Two rects: `<rect width="480" height="480" fill="url(#bgG)"/>` (the gradient backdrop, REMOVE) and `<rect width="480" height="480" fill="url(#scanlines)"/>` (the cosmetic scanline overlay, KEEP — it's intentional and will compose nicely on top of any background).

**Tests:**
- `tests/config_tests.rs` — Extend with `[background]` section parsing tests.
- `tests/dbus_tests.rs` — Extend with `validate_background_path` traversal tests + `apply_background` triple-effect test (persist + in-memory + ModeChange::Background).
- `tests/render_tests.rs` — Add a test that `SvgRenderer::set_background(Some(bg))` produces a composited frame whose pixel at (0,0) matches the bg color when the layout has a transparent canvas.

### Research Findings

- **`tiny_skia::Pixmap::draw_pixmap` signature** (tiny-skia 0.12): `pub fn draw_pixmap(&mut self, x: i32, y: i32, pixmap: PixmapRef, paint: &PixmapPaint, transform: Transform, mask: Option<&Mask>) -> Option<()>`. Returns `None` if compositing is impossible (e.g., zero-size). Use `PixmapPaint::default()` for normal alpha-over (BlendMode::SourceOver).
- **Pixmap stores premultiplied RGBA**. Same gotcha as the GUI's `putImageData` work — `Pixmap::from_vec(data, IntSize)` expects premultiplied. For a fully-opaque image (alpha=255 everywhere) premultiplied == straight, no math needed. PNG/JPEG decoded backgrounds are typically fully opaque.
- **`image` crate** (already a dep, version 0.25 with PNG/JPEG features). Use `image::open(path)?` → `DynamicImage::into_rgba8()` → `ImageBuffer<Rgba<u8>, Vec<u8>>`. Then convert to Pixmap by passing `into_raw()` Vec into `Pixmap::from_vec(vec, IntSize::from_wh(w, h)?).ok_or(...)`. For non-opaque source images, premultiply manually before `from_vec`.
- **Background image dimensions:** the LCD is 480x480. Decoded bg should be resized to 480x480 to skip per-tick scaling. Use `image::imageops::resize(img, 480, 480, FilterType::Lanczos3)` once at decode time.
- **D-Bus method casing:** Rust `set_background` becomes D-Bus `SetBackground`. Rust `vars: HashMap<String, String>` becomes D-Bus `a{ss}`. Rust `Option<String>` is *not* directly serializable over D-Bus — pass empty string for "clear". Or split into separate `SetBackground(name: String)` and `ClearBackground()` methods (cleaner, matches design doc).
- **`#[serde(default)]` on Option fields**: needed so existing config files without `[background]` keep loading. Test: load a fixture without `[background]` section — must succeed with `image: None`.
- **ModeChange Pixmap payload**: `tiny_skia::Pixmap` is `Send` but sending it through the mpsc channel works fine. The decode happens in the D-Bus method handler (off the bus thread, on the tokio worker pool).
- **First-run seed:** ship 2 default backgrounds in `layouts/backgrounds/` (or `assets/backgrounds/`) sourced via `include_bytes!`. Pick simple, dark, low-distraction images — the LCD washes out detail anyway. Suggested: a solid dark gradient and a subtle hex-pattern. Final choice during execution.

### Relevant Patterns

- `src/service/dbus.rs:204-330` — Triple-effect mutation pattern: `apply_layout_vars()` validates path, persists via `Config::save_*`, mutates in-memory state, then sends `ModeChange::*` over the channel. Mirror exactly for `apply_background()`.
- `src/config.rs:127-196` — `Config::save_layout_vars` using `toml_edit::DocumentMut::from_str`, mutating a single section, atomic write via temp file + rename in same directory. Mirror as `Config::save_background_image(path, name: Option<&str>)`.
- `src/config.rs:269-310` — `seed_layout_dir`: `include_str!` for content, write only if `!dest.exists()`, create parent dirs. Mirror as `seed_background_dir` using `include_bytes!`.
- `gui/src-tauri/src/commands.rs:71-90` — `list_layouts` shape: directory scan, filename collection. Mirror for `list_backgrounds`.
- `gui/src-tauri/src/commands.rs:295-318` — `validate_layout_path`: canonicalize both base + resolved, then `starts_with`. Use the same pattern (or generalize into one helper that takes the base dir).

## Execution Architecture

**Team:** 2 devs, 1 spec reviewer, 1 quality reviewer

**Task dependencies:**
- Task 1 (preflight bug fix) is independent and unblocks the rest.
- Tasks 2-9 (Phase 1: daemon backend) are mostly sequential — config, then render, then D-Bus.
- Task 10-12 (Phase 2: layouts) is independent of Phase 1 and can run in parallel with daemon work after Task 1 lands.
- Tasks 13-17 (Phase 3: GUI) depend on Phase 1 D-Bus methods (Task 8) being merged.
- Task 18-19 (Phase 4: hardware) depends on Phases 1+2+3.

**Parallelism:** dev-1 takes Phase 1 daemon work; dev-2 can take Phase 2 (layouts) in parallel after Task 1 lands. They converge before Phase 3.

**Phases:**
- Phase 0: Task 1 (preflight bug fix — daemon reload path)
- Phase 1: Tasks 2-9 (daemon backend: config, decode/cache, compositing, D-Bus)
- Phase 2: Tasks 10-12 (layout updates)
- Phase 3: Tasks 13-17 (GUI integration)
- Phase 4: Tasks 18-19 (hardware verification)

**Milestones:**
- After Task 9 (Phase 1 done — daemon can show a bg from a fixed config setting)
- After Task 12 (Phase 2 done — layouts allow bg to show through)
- After Task 17 (Phase 3 done — GUI gallery functional)
- After Task 19 (final — hardware verified end-to-end)

---

## Phase 0: Preflight Bug Fix

### Task 1: Fix daemon reload path to preserve theme + history [DO-CONFIRM]

**Files:**
- Modify: `src/main.rs:186-219` (ModeChange::Layout handler)
- Test: `tests/render_tests.rs` (extend or create a regression test)

**Why this exists:** Discovered during planning. The mode-change handler at `src/main.rs:196-200` constructs a new `SvgRenderer` with `ThemePalette::default()` and never calls `set_history()`. This causes Tera template substitution failures when a layout uses `{{ cpu_temp_history }}` or any `{% set %}` based on a theme color from `[theme.manual]`. Surfaced as the "Apply broke the LCD" symptom. Fix is small and shares files with the rest of this plan, so it ships first.

**Implement:**
1. Add `theme: ThemePalette` and `sensor_history: Option<Arc<Mutex<SensorHistory>>>` to the closure captured by the `tokio::spawn` at `src/main.rs:182`. They must be cloned/moved into the spawn, not borrowed.
2. In the `ModeChange::Layout { name, vars }` arm at line 188, after constructing the new `SvgRenderer`:
   - Call `r.set_theme(theme.clone())` instead of `r.set_theme(ThemePalette::default())`.
   - Call `r.set_history(history.clone())` if `sensor_history` is `Some`.
   - Same applies to the `set_mode` D-Bus path that also builds an SvgRenderer (verify and fix all sites).

**Confirm checklist:**
- [ ] Failing test written FIRST: a test that simulates `ModeChange::Layout` and asserts the resulting SvgRenderer has both the configured theme palette and the attached sensor history (not defaults). Use a tempdir-backed test fixture with a layout that references `{{ theme_primary }}` and `{{ cpu_temp_history }}`.
- [ ] After the fix, the test passes.
- [ ] No regression: full `cargo test --workspace` passes (153/153 baseline).
- [ ] Manual verification: rebuild + restart daemon, click Apply in GUI, confirm the journal has no "Tera template substitution failed" warnings.
- [ ] Commit: `fix(daemon): preserve theme palette and sensor history on layout reload`

### Task 2: Review Task 1

**Trigger:** Both reviewers start when Task 1 completes.

**Killer items (blocking):**
- [ ] `src/main.rs:198` no longer uses `ThemePalette::default()` — it uses the configured theme from `config.theme.manual` (or `ThemePalette::default()` only as fallback when `manual` is `None`)
- [ ] `src/main.rs` mode-change handler calls `set_history()` on the new renderer when sensor_history is configured
- [ ] Both `ModeChange::Layout` and `ModeChange::Xvfb→layout` paths (if applicable) preserve theme + history
- [ ] Test asserts on configured-theme values (not just "is not None"): e.g., `assert_eq!(renderer.theme().primary, "#7aa2f7")`
- [ ] Test asserts history is attached: rendering a `{{ cpu_temp_history }}`-referencing template succeeds without Tera error
- [ ] No `clone()` chain that defeats `Arc` reference-counting (check `Arc<Mutex<SensorHistory>>` is cloned, not its inner value)

**Quality items (non-blocking):**
- [ ] Theme construction logic is shared between initial-load and reload paths (DRY) — not duplicated
- [ ] No new clippy warnings in `src/main.rs`

---

## Phase 1: Daemon Backend

### Task 3: Add `[background]` config schema + delete dead `theme.background_image` [DO-CONFIRM]

**Files:**
- Modify: `src/config.rs:75-90` (delete `ThemeConfig.background_image` field)
- Modify: `src/config.rs:83-114` (add `BackgroundConfig` struct + `pub background: BackgroundConfig` field on `Config`)
- Test: `tests/config_tests.rs` (parse fixture with and without `[background]` section)

**Implement:**

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct BackgroundConfig {
    /// Filename (no path) of the active background under
    /// ~/.config/thermalwriter/backgrounds/. Empty/None = no background.
    pub image: Option<String>,
}
```

Add `pub background: BackgroundConfig` to `Config`. Delete `background_image: Option<String>` from `ThemeConfig`.

**Confirm checklist:**
- [ ] Failing test written FIRST: parse a TOML fixture with `[background] image = "skyline.png"`, assert `config.background.image == Some("skyline.png".to_string())`
- [ ] Second failing test FIRST: parse a fixture WITHOUT `[background]` section, assert `config.background.image == None` (proves backwards compat)
- [ ] Third failing test FIRST: parse a fixture without `[theme]` section, confirm Config still loads (deletion of `theme.background_image` doesn't break)
- [ ] `theme.background_image` field is fully gone — grep `background_image` outside `BackgroundConfig` returns no hits
- [ ] All existing tests still pass: `cargo test --workspace`
- [ ] Commit: `feat(config): add [background] section, remove dead theme.background_image`

### Task 4: Review Task 3

**Trigger:** Both reviewers start when Task 3 completes.

**Killer items (blocking):**
- [ ] `BackgroundConfig` is `#[serde(default)]` so missing section parses cleanly
- [ ] `image: Option<String>` not `image: String` — the "no background" state is representable
- [ ] Existing config files load without errors (run `cargo run -- daemon` mentally — would the user's current config.toml without [background] still parse?)
- [ ] `theme.background_image` field is deleted (not just unused) — grep returns clean
- [ ] At least 3 tests added in `tests/config_tests.rs` covering: with section, without section, with `image = None` explicit

**Quality items (non-blocking):**
- [ ] Doc comment on `BackgroundConfig` mentions where the file lives on disk (`~/.config/thermalwriter/backgrounds/`)
- [ ] Field name `image` not `path` or `filename` (clearest in TOML: `[background] image = "..."`)

### Task 5: Background decode + cache module [READ-DO]

**Files:**
- Create: `src/render/background.rs` (new module)
- Modify: `src/render/mod.rs` (add `pub mod background;`)
- Modify: `src/config.rs` (add `seed_background_dir` mirroring `seed_layout_dir`, add `Config::save_background_image`)
- Modify: `Cargo.toml` if any dep adjustment (likely none — `image` is already there)
- Create: `assets/backgrounds/` with 1-2 small seed PNGs
- Test: `tests/render_tests.rs` (decode roundtrip test)

**Step 1: Invoke `forge:writing-tests` skill**

> Before writing any test, invoke the skill. The byte-count and meaningful-assertion guidance applies — assert on actual pixel values, not just "decoded successfully."

**Step 2: Write the failing test**

In `tests/render_tests.rs`:

```rust
#[test]
fn decode_png_to_pixmap_roundtrips_dimensions() {
    // 480x480 solid red PNG, embedded as test fixture or generated inline
    let bytes: Vec<u8> = make_solid_red_png(480, 480);
    let pixmap = thermalwriter::render::background::decode_to_pixmap(&bytes)
        .expect("decode succeeds");
    assert_eq!(pixmap.width(), 480);
    assert_eq!(pixmap.height(), 480);
    // Center pixel is red (premultiplied: R=255, G=0, B=0, A=255)
    let idx = (240 * 480 + 240) * 4;
    assert_eq!(pixmap.data()[idx], 255);     // R
    assert_eq!(pixmap.data()[idx + 1], 0);   // G
    assert_eq!(pixmap.data()[idx + 2], 0);   // B
    assert_eq!(pixmap.data()[idx + 3], 255); // A
}

#[test]
fn decode_resizes_non_480_input_to_480() {
    let bytes: Vec<u8> = make_solid_red_png(800, 600);
    let pixmap = thermalwriter::render::background::decode_to_pixmap(&bytes)
        .expect("decode succeeds");
    assert_eq!(pixmap.width(), 480);
    assert_eq!(pixmap.height(), 480);
}
```

The `make_solid_red_png` helper can use the `image` crate to encode a tiny PNG in-memory.

**Step 3: Run tests, confirm they fail**

`cargo test --test render_tests decode_` should fail with "module `background` not found" or "function `decode_to_pixmap` not defined".

**Step 4: Implement `src/render/background.rs`**

```rust
// src/render/background.rs
use anyhow::{Context, Result};
use image::imageops::FilterType;
use tiny_skia::{IntSize, Pixmap};

/// Target LCD dimensions. Backgrounds are resized to this on decode so
/// subsequent compositing is a straight blit with no per-tick scaling.
const LCD_W: u32 = 480;
const LCD_H: u32 = 480;

/// Decode a background image from raw bytes (PNG/JPEG) into a 480x480
/// premultiplied-RGBA Pixmap, ready for compositing under a layout.
///
/// - Resizes any input dimensions to LCD_W × LCD_H using Lanczos3.
/// - Premultiplies alpha so the result satisfies tiny_skia's contract.
/// - Returns Err for unsupported formats or decode failures.
pub fn decode_to_pixmap(bytes: &[u8]) -> Result<Pixmap> {
    let img = image::load_from_memory(bytes)
        .context("Failed to decode background image (unsupported format?)")?;
    let resized = image::imageops::resize(&img.into_rgba8(), LCD_W, LCD_H, FilterType::Lanczos3);

    // Premultiply alpha. For fully-opaque images (alpha=255) this is a no-op
    // numerically, but we run it unconditionally to handle PNGs with alpha.
    let mut data = resized.into_raw();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        px[0] = ((px[0] as u32 * a) / 255) as u8;
        px[1] = ((px[1] as u32 * a) / 255) as u8;
        px[2] = ((px[2] as u32 * a) / 255) as u8;
    }

    let size = IntSize::from_wh(LCD_W, LCD_H)
        .ok_or_else(|| anyhow::anyhow!("invalid LCD size constants"))?;
    Pixmap::from_vec(data, size)
        .ok_or_else(|| anyhow::anyhow!("Pixmap::from_vec rejected RGBA buffer"))
}

/// Decode a background by filename from a known directory. Used by the
/// daemon at startup and on D-Bus SetBackground.
pub fn decode_from_file(path: &std::path::Path) -> Result<Pixmap> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read background file: {}", path.display()))?;
    decode_to_pixmap(&bytes)
}
```

**Step 5: Run tests, confirm they pass**

`cargo test --test render_tests decode_` should pass.

**Step 6: Add `seed_background_dir` to `src/config.rs`**

Mirror `seed_layout_dir` at `src/config.rs:269`. Use `include_bytes!` for binary PNG content. Place 1-2 default PNGs in `assets/backgrounds/` (create the dir). Suggested: `dark-solid.png` (a flat #08080f square) and `hex-grid.png` (a subtle hex pattern). Final choice during execution — keep them small (~10 KB each).

**Step 7: Add `Config::save_background_image(path, image: Option<&str>)` to `src/config.rs`**

Mirror `Config::save_layout_vars` at line 127. Use `toml_edit::DocumentMut`, mutate `[background].image`, atomic write. When `image` is `Some`, set the value; when `None`, remove the key (or set to empty string — match what the read path expects).

Test in `tests/config_tests.rs`: comment-preservation roundtrip with `[background]` section.

**Step 8: Run all tests + commit**

```bash
cargo test --workspace
git add src/render/background.rs src/render/mod.rs src/config.rs assets/backgrounds/ tests/render_tests.rs tests/config_tests.rs Cargo.toml
git commit -m "feat(render): background image decode + cache module"
```

### Task 6: Review Task 5

**Trigger:** Both reviewers start when Task 5 completes.

**Killer items (blocking):**
- [ ] `decode_to_pixmap` premultiplies alpha unconditionally (verify the math at `background.rs:~25`)
- [ ] Resize to 480×480 happens at decode time, not per-tick
- [ ] Test asserts on actual pixel values (`pixmap.data()[idx] == 255`), not just dimensions
- [ ] Non-480 input is resized (test `decode_resizes_non_480_input_to_480` exists and passes)
- [ ] `Config::save_background_image` uses `toml_edit::DocumentMut` (NOT `toml::to_string`)
- [ ] Atomic write: temp file in same dir + rename (mirrors save_layout_vars at `src/config.rs:163-194`)
- [ ] `seed_background_dir` only writes if `!dest.exists()` (don't overwrite user customizations)
- [ ] No `unwrap()` in production paths in `background.rs`

**Quality items (non-blocking):**
- [ ] Both PNG and JPEG decode succeed (add a JPEG test if not present)
- [ ] Filter choice (Lanczos3) documented in comment with rationale
- [ ] Seed PNG file sizes < 50 KB each (binary bloat to the daemon binary)
- [ ] Module doc on `src/render/background.rs` explaining the premultiply gotcha

### Task 7: Wire compositing into `SvgRenderer` [READ-DO]

**Files:**
- Modify: `src/render/svg.rs:36-91` (impl block — add `set_background`, store field)
- Modify: `src/render/svg.rs:93-145` (FrameSource::render — add compositing)
- Test: `tests/render_tests.rs` (compositing test)

**Step 1: Write the failing test**

```rust
#[test]
fn svg_renderer_composites_background_under_transparent_layout() {
    // Solid red bg
    let bg = thermalwriter::render::background::decode_to_pixmap(&make_solid_red_png(480, 480))
        .unwrap();

    // Minimal layout with no canvas-fill rect — fully transparent canvas
    let template = r#"<svg viewBox="0 0 480 480" xmlns="http://www.w3.org/2000/svg">
        <text x="240" y="240" fill="#ffffff" text-anchor="middle">hi</text>
    </svg>"#;

    let mut renderer = SvgRenderer::new(template, 480, 480).unwrap();
    renderer.set_background(Some(bg));

    let frame = renderer.render(&Default::default()).unwrap();
    // Pixel at (0, 0) — top-left, no text — should be the bg's red.
    let idx = 0;
    assert_eq!(frame.data[idx], 255);     // R
    assert_eq!(frame.data[idx + 1], 0);   // G
    assert_eq!(frame.data[idx + 2], 0);   // B
}

#[test]
fn svg_renderer_renders_normally_without_background() {
    let template = r#"<svg viewBox="0 0 480 480" xmlns="http://www.w3.org/2000/svg">
        <rect width="480" height="480" fill="#0000ff"/>
    </svg>"#;

    let mut renderer = SvgRenderer::new(template, 480, 480).unwrap();
    // No set_background call — should still render fine.
    let frame = renderer.render(&Default::default()).unwrap();
    let idx = 0;
    assert_eq!(frame.data[idx + 2], 255); // B = 255 (the rect's blue)
}
```

**Step 2: Run tests — confirm they fail**

Compile error: `set_background` not defined.

**Step 3: Add the field + setter**

In `src/render/svg.rs:36-91`, add to the struct:

```rust
pub struct SvgRenderer<'a> {
    // ... existing fields ...
    background: Option<tiny_skia::Pixmap>,
}
```

In the impl block, initialize `background: None` in `new()`. Add:

```rust
/// Set or clear the background image. The image must be 480×480 and
/// premultiplied — typically produced via `crate::render::background::decode_to_pixmap`.
pub fn set_background(&mut self, bg: Option<tiny_skia::Pixmap>) {
    self.background = bg;
}
```

**Step 4: Composite in `render()`**

In `src/render/svg.rs:93-145`, find where the layout is rendered into a `pixmap` (around line 134). Replace:

```rust
let mut pixmap = Pixmap::new(self.width, self.height).context(...)?;
// ... resvg::render(...) into pixmap ...
Ok(RawFrame::from_pixmap(&pixmap))
```

with (conceptually):

```rust
// Render layout to its own transparent pixmap.
let mut layout_pixmap = Pixmap::new(self.width, self.height).context(...)?;
resvg::render(&tree, transform, &mut layout_pixmap.as_mut());

// If a background is set, blit it as the base, then layout on top.
let final_pixmap = if let Some(ref bg) = self.background {
    let mut composed = bg.clone();
    composed.draw_pixmap(
        0, 0,
        layout_pixmap.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        tiny_skia::Transform::identity(),
        None,
    );
    composed
} else {
    layout_pixmap
};

Ok(RawFrame::from_pixmap(&final_pixmap))
```

Note: `bg.clone()` is a 921600-byte memcpy per tick. At 2 FPS that's ~2 MB/s — negligible. At 15 FPS xvfb mode, ~14 MB/s — still fine. If perf testing shows it's a hotspot, the future optimization is to pre-allocate a scratch pixmap and `bg.draw_pixmap` into the scratch then layout on top.

**Step 5: Run tests, confirm they pass**

`cargo test --test render_tests svg_renderer_` should pass.

**Step 6: Run full suite**

`cargo test --workspace` — must stay green (no daemon-side regression).

**Step 7: Commit**

```bash
git commit -m "feat(render): composite background under SvgRenderer output"
```

### Task 8: Review Task 7

**Trigger:** Both reviewers start when Task 7 completes.

**Killer items (blocking):**
- [ ] `SvgRenderer.background` is `Option<Pixmap>` — initialized to `None` in `new()` so existing callers don't need to change
- [ ] `set_background` is `pub fn` (callable from main.rs and tests)
- [ ] Compositing path uses `PixmapPaint::default()` (alpha-over) — NOT a custom blend mode
- [ ] Composited output produces a `RawFrame` with the SAME byte count as before (480×480×3 = 691200 RGB) — `RawFrame::from_pixmap` handles the unpremultiply
- [ ] Test `svg_renderer_composites_background_under_transparent_layout` asserts on actual pixel RGB values — not just frame size
- [ ] Test `svg_renderer_renders_normally_without_background` proves the no-background path is unchanged
- [ ] `cargo test --workspace` is still 153/153 (daemon side)

**Quality items (non-blocking):**
- [ ] `bg.clone()` per-tick allocation noted in a comment (not a regression for V1, but flagged for future optimization)
- [ ] Field name `background` (not `bg_image` or `bg_pixmap`) — matches public API of `set_background`

### Task 9: D-Bus methods for background control [DO-CONFIRM]

**Files:**
- Modify: `src/service/dbus.rs:19-23` (extend `ModeChange` enum with `Background` variant)
- Modify: `src/service/dbus.rs:35-50` (extend `ServiceState` with `current_background` and `background_dir`)
- Modify: `src/service/dbus.rs:55-150` (add `SetBackground`/`ClearBackground`/`ListBackgrounds` D-Bus methods)
- Modify: `src/service/dbus.rs:200-220` (add `validate_background_path` helper; or generalize `validate_layout_path`)
- Modify: `src/main.rs:139-153` (call `renderer.set_background(decoded)` on initial construction)
- Modify: `src/main.rs:186-219` (handle `ModeChange::Background` in the spawned task; also propagate the current bg to layout-rebuild path)
- Modify: `src/dbus_types.rs` (extend `DisplayProxy` trait with new methods)
- Modify: `src/cli.rs` (add `Background` subcommand for parity with `set_layout` etc., optional but nice)
- Test: `tests/dbus_tests.rs` (path traversal + apply-background triple-effect tests)

**Implement:**

1. **`ModeChange::Background { image: Option<tiny_skia::Pixmap> }`** — added variant. The handler in `src/main.rs:186-219` matches it and calls `renderer.set_background(image)` on the current frame_source if it's an SvgRenderer (the source is `Box<dyn FrameSource>` so you'll need a downcast helper or a trait method). **Choice:** add `fn set_background(&mut self, _bg: Option<tiny_skia::Pixmap>) {}` as a default no-op on `FrameSource` trait. SvgRenderer overrides. TemplateRenderer keeps the no-op (HTML doesn't get backgrounds in V1).
2. **`ServiceState.current_background: Option<tiny_skia::Pixmap>` + `background_dir: PathBuf`** — populated at startup from config + filesystem.
3. **D-Bus `SetBackground(name: String) -> ()`**: validates path under `background_dir`, decodes via `crate::render::background::decode_from_file`, persists `[background].image = name` via `Config::save_background_image`, mutates in-memory `state.current_background = Some(decoded)`, sends `ModeChange::Background { image: Some(decoded.clone()) }`.
4. **D-Bus `ClearBackground() -> ()`**: persists `[background].image = None`, mutates `state.current_background = None`, sends `ModeChange::Background { image: None }`.
5. **D-Bus `ListBackgrounds() -> Vec<String>`**: directory listing of `background_dir`, returns filenames. PNG/JPEG only.
6. **Triple-effect helper** `apply_background()` mirroring `apply_layout_vars` — see `src/service/dbus.rs:204-330`.
7. **Layout-rebuild propagation:** when ModeChange::Layout fires, after constructing the new SvgRenderer, also call `renderer.set_background(state.current_background.clone())` so the bg survives layout changes.

**Confirm checklist:**
- [ ] Failing tests written FIRST in `tests/dbus_tests.rs`:
  - `validate_background_path` rejects `..`, absolute paths, and symlink-escape (3 separate cases — same shape as `validate_layout_path` tests)
  - `apply_background` does triple-effect: persists to config, updates `state.current_background`, sends `ModeChange::Background` over channel
  - `apply_background` with `None` clears all three
- [ ] `ModeChange::Background` variant added; payload is `Option<Pixmap>`
- [ ] `FrameSource` trait gains a default-no-op `set_background` method; `SvgRenderer` overrides
- [ ] `ServiceState.current_background` initialized at startup if `config.background.image` is set
- [ ] D-Bus methods registered (introspect via `busctl --user introspect com.thermalwriter.Service /com/thermalwriter/display` after running — must show `SetBackground`, `ClearBackground`, `ListBackgrounds`)
- [ ] `DisplayProxy` trait in `src/dbus_types.rs` extended to mirror server-side
- [ ] Layout-rebuild path also re-applies current_background (so Apply-changing-layout doesn't drop the bg)
- [ ] No `unwrap()` in D-Bus method bodies
- [ ] Path traversal: canonicalize + starts_with (NOT contains(".."))
- [ ] `cargo test --workspace` still green
- [ ] Commit: `feat(dbus): add SetBackground/ClearBackground/ListBackgrounds`

### Task 10: Review Task 9

**Trigger:** Both reviewers start when Task 9 completes.

**Killer items (blocking):**
- [ ] `validate_background_path` uses canonicalize + starts_with, not string check
- [ ] Path traversal test cases pass: `../../etc/passwd`, absolute paths, symlink-escape all rejected
- [ ] `apply_background` triple-effect test passes: persistence + in-memory + ModeChange (all three side effects in one call)
- [ ] `Config::save_background_image` uses `toml_edit::DocumentMut`, atomic write same dir
- [ ] `FrameSource::set_background` default impl is a no-op (so TemplateRenderer doesn't need code changes); SvgRenderer overrides
- [ ] Layout-rebuild path (in `src/main.rs:186-219`) calls `renderer.set_background(state.current_background.clone())` after construction so layout switches preserve the bg
- [ ] `busctl introspect` shows the three new methods (manual verification, called out in commit message)
- [ ] DisplayProxy trait in `src/dbus_types.rs` matches server signatures

**Quality items (non-blocking):**
- [ ] `validate_background_path` is shared with or generalized from `validate_layout_path` (DRY) — not a third copy of the same logic
- [ ] D-Bus method signatures: `SetBackground(s) -> ()`, `ClearBackground() -> ()`, `ListBackgrounds() -> as`
- [ ] Test for "background file doesn't exist" path — D-Bus method returns `zbus::fdo::Error`, doesn't panic
- [ ] CLI `thermalwriter ctl set-background <name>` subcommand added (for parity with `set-layout` — optional but nice)

### Task 11: Milestone — Daemon backend complete

**Present to user:**
- Phase 0+1 work shipped: bug fix in reload path, [background] config schema, decode/cache module, compositing wired into SvgRenderer, three D-Bus methods.
- Run `busctl --user introspect com.thermalwriter.Service /com/thermalwriter/display` — shows `SetBackground`, `ClearBackground`, `ListBackgrounds`.
- Manual verification: from CLI, `thermalwriter ctl set-background <seeded-default>.png` (if Task 9 added the CLI subcommand) and the LCD shows the bg under the current layout. (At this milestone the seeded layouts still have opaque rects, so the bg may or may not be visible depending on which layout is active and how its theme_background renders. Phase 2 fixes the layouts.)
- All tests green.

**Wait for user response before proceeding to Phase 2.**

---

## Phase 2: Layout Updates

### Task 12: Remove full-canvas opaque rects from seeded layouts [DO-CONFIRM]

**Files:**
- Modify: `layouts/svg/neon-dash-v2.svg:22` (delete the full-canvas rect)
- Modify: `layouts/svg/neon-dash.svg:29` (delete)
- Modify: `layouts/svg/arc-gauge.svg:49` (delete)
- Modify: `layouts/svg/cyber-grid.svg:48` (delete the `bgG` gradient rect; KEEP the scanlines rect at line 49)
- Test: visual via `cargo run --example preview_layout layouts/svg/<name>.svg`

**Implement:**

For each of the four SVG layout files, delete the full-canvas opaque background rect identified in the line numbers above. Per-panel rects (sized smaller than 480×480 or using `url(#panelGrad)`) STAY. The scanline overlay in cyber-grid (line 49) STAYS — it's intentional cosmetics that compose nicely on top of any bg.

**Confirm checklist:**
- [ ] Failing test written FIRST: `cargo run --example preview_layout layouts/svg/neon-dash-v2.svg` produces a PNG with mostly-transparent (or layout-default-color) canvas — assert the top-left pixel is NOT the user's `theme_background` color anymore
- [ ] All 4 seeded SVG files no longer have a `<rect width="480" height="480" fill="..."/>` first-painting rect
- [ ] cyber-grid.svg STILL has the scanlines overlay (`url(#scanlines)`) — KEEP that one
- [ ] Each layout still renders standalone via `cargo run --example preview_layout` (no Tera errors, panels still visible)
- [ ] When the daemon's bg cache has a value, the bg shows through behind the panels (verify via `cargo test --test render_tests`)
- [ ] `cargo test --workspace` still green
- [ ] Commit: `refactor(layouts): remove full-canvas opaque rects so global bg shows through`

### Task 13: Review Task 12

**Trigger:** Both reviewers start when Task 12 completes.

**Killer items (blocking):**
- [ ] All 4 seeded SVGs no longer have a full-canvas (480×480) opaque first rect
- [ ] cyber-grid.svg STILL has its scanlines pattern rect (intentional)
- [ ] All 4 layouts still render via `cargo run --example preview_layout` without Tera errors
- [ ] Panel rects (CPU, GPU, RAM cards) are unchanged — they're sized smaller than 480×480
- [ ] Frontmatter (`{# vars: #}` block) is unchanged — only the SVG body is edited

**Quality items (non-blocking):**
- [ ] Unused `<defs>` (e.g., the `bgGrad` linearGradient in arc-gauge that's no longer referenced) — cleanup is fine but optional
- [ ] No layout has accidentally lost its panel backgrounds (the `url(#panelGrad)` rects must survive)

### Task 14: Milestone — Layouts updated

**Present to user:**
- 4 seeded SVG layouts now have transparent canvases (panels still visible, scanlines preserved on cyber-grid).
- With the daemon's bg cache set (manual CLI test), the bg image shines through behind the panels.
- User's existing customized 2 MB `~/.config/thermalwriter/layouts/svg/neon-dash-v2.svg` is untouched. They can replace it with the updated seeded version (`cp layouts/svg/neon-dash-v2.svg ~/.config/thermalwriter/layouts/svg/`) when convenient — until then, their inline-bg layout works as before but the global bg won't show through it.

**Wait for user response before proceeding to Phase 3.**

---

## Phase 3: GUI Integration

### Task 15: Tauri commands for backgrounds [DO-CONFIRM]

**Files:**
- Modify: `gui/src-tauri/src/commands.rs:71-238` (add `list_backgrounds`, `set_background`, `get_active_background`)
- Modify: `gui/src-tauri/src/lib.rs:27-39` (register in `generate_handler!`)
- Modify: `gui/src-tauri/src/error.rs` if needed (likely fold into existing variants — `LayoutIo` already covers fs errors)
- Test: `gui/src-tauri/src/commands.rs` `#[cfg(test)]` mod (path-traversal + list-roundtrip)

**Implement:**

```rust
#[tauri::command]
pub fn list_backgrounds(state: tauri::State<'_, RendererState>) -> Result<Vec<String>, AppError> {
    // Mirror list_layouts at commands.rs:71. Read state.background_dir (a new field
    // on RendererState — add it in lib.rs's setup hook), filter for .png/.jpg, sort.
    todo!()
}

#[tauri::command]
pub async fn set_background(
    name: Option<String>,
    state: tauri::State<'_, RendererState>,
) -> Result<(), AppError> {
    // None → call DisplayProxy::clear_background()
    // Some(name) → validate name under state.background_dir, then DisplayProxy::set_background(name)
    // On daemon-not-running: AppError::DaemonUnavailable, frontend handles fallback
    todo!()
}

#[tauri::command]
pub fn get_active_background(state: tauri::State<'_, RendererState>) -> Result<Option<String>, AppError> {
    // Read [background].image from config.toml (similar to get_saved_vars).
    todo!()
}
```

`RendererState` (in `gui/src-tauri/src/lib.rs`) gains a `background_dir: PathBuf` field, populated at setup from `~/.config/thermalwriter/backgrounds/`.

**Confirm checklist:**
- [ ] Failing tests written FIRST: `list_backgrounds` returns sorted PNG/JPEG filenames from a tempdir; `set_background(None)` calls `DisplayProxy::clear_background`; path-traversal rejection
- [ ] All three commands return `Result<T, AppError>` (uniform with existing commands)
- [ ] `set_background` daemon-unavailable returns `AppError::DaemonUnavailable` (not panic, not wrapped String)
- [ ] All three registered in `generate_handler!` at `lib.rs:27-39`
- [ ] No `unwrap()` in command bodies (test bodies OK)
- [ ] `cargo build -p thermalwriter-gui` exit 0
- [ ] `cargo test -p thermalwriter-gui` all pass
- [ ] Commit: `feat(gui): Tauri commands for background management`

### Task 16: Review Task 15

**Trigger:** Both reviewers start when Task 15 completes.

**Killer items (blocking):**
- [ ] All 3 commands return `Result<T, AppError>` consistently (NOT `Result<T, String>`)
- [ ] `list_backgrounds` filters by extension (.png, .jpg, .jpeg) — not just any file
- [ ] `set_background(None)` correctly invokes `DisplayProxy::clear_background()` (NOT `set_background("")`)
- [ ] Path traversal: canonicalize + starts_with (mirror `validate_layout_path` at `commands.rs:295-318`)
- [ ] Daemon-not-running returns `AppError::DaemonUnavailable` with descriptive reason
- [ ] All 3 registered in `generate_handler!` (verify at `lib.rs:27-39`)
- [ ] `cargo test -p thermalwriter-gui` includes 5+ new tests covering list, set, set-clear, get-active, and path traversal

**Quality items (non-blocking):**
- [ ] Path validation helper is shared with the existing `validate_layout_path` (DRY) — generalized into one helper
- [ ] No `unwrap()` outside test mod
- [ ] Error variants in `AppError` reused from existing set unless background-specific semantics genuinely differ

### Task 17: Svelte gallery panel + Apply-flow extension [READ-DO]

**Files:**
- Create: `gui/src/lib/BgGallery.svelte` (new component)
- Modify: `gui/src/App.svelte` (mount BgGallery, extend Apply flow to include background)

**Step 1: Invoke `forge:writing-tests` skill** (frontend smoke verification, not unit tests in Svelte unless you have a runner)

**Step 2: Create `gui/src/lib/BgGallery.svelte`**

```svelte
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let { selected = $bindable<string | null>() } = $props();

  let backgrounds: string[] = $state([]);
  let loading = $state(true);
  let error = $state("");

  async function load() {
    loading = true;
    error = "";
    try {
      backgrounds = await invoke<string[]>("list_backgrounds");
      // Read currently-active background and set it as selected on first load.
      if (selected === undefined) {
        const active = await invoke<string | null>("get_active_background");
        selected = active;
      }
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    load();
  });
</script>

<div class="bg-gallery">
  <h3>Background</h3>
  {#if loading}
    <p>Loading…</p>
  {:else if error}
    <p class="error">{error}</p>
  {:else}
    <div class="grid">
      <button
        class="tile none"
        class:active={selected === null}
        onclick={() => (selected = null)}
      >
        None
      </button>
      {#each backgrounds as name}
        <button
          class="tile"
          class:active={selected === name}
          onclick={() => (selected = name)}
          title={name}
        >
          <img
            src={`http://ipc.localhost/__background_thumb/${encodeURIComponent(name)}`}
            alt={name}
            loading="lazy"
          />
          <span>{name}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .bg-gallery {
    /* match existing layout-list styling */
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(96px, 1fr));
    gap: 8px;
  }
  .tile {
    aspect-ratio: 1;
    border: 2px solid transparent;
    background: #1a1b26;
    color: #c0caf5;
    cursor: pointer;
  }
  .tile.active {
    border-color: #7aa2f7;
  }
  .tile img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
</style>
```

**Note on thumbnails:** the `src` URL above uses a Tauri custom protocol pattern. If that's not yet wired up, simplify the V1 to NOT show thumbnails — just the filename text on each tile. Real thumbnails can be a follow-up. Acceptable scope reduction since the design allows for "tracked as future enhancement."

**Step 3: Wire BgGallery into App.svelte**

Add a new state binding `let bgSelected: string | null = $state(null);`, mount `<BgGallery bind:selected={bgSelected} />` next to the layout picker, and extend the existing `apply()` function:

```typescript
async function apply() {
  // ... existing save_config / apply_to_daemon flow ...

  // Then: apply background change.
  try {
    await invoke<void>("set_background", { name: bgSelected });
  } catch (e) {
    // If apply_to_daemon already failed with DaemonUnavailable, this will too.
    // Don't double-display; let the existing status flow handle it.
  }
}
```

**Step 4: Manual launch verification**

`cd gui && npm run tauri dev` → window opens → BgGallery shows the seeded backgrounds + a "None" tile → clicking a tile and Apply pushes the bg to the daemon (verify `busctl monitor com.thermalwriter.Service` sees `SetBackground` traffic).

**Step 5: Commit**

```bash
git commit -m "feat(gui): background gallery panel + Apply integration"
```

### Task 18: Review Task 17

**Trigger:** Both reviewers start when Task 17 completes.

**Killer items (blocking):**
- [ ] BgGallery uses Svelte 5 runes (`$state`, `$effect`, `$props`, `$bindable`) — NOT legacy `$:` reactive statements
- [ ] "None" tile clears the bg correctly (calls `set_background({ name: null })`)
- [ ] `bind:selected` flow: parent App.svelte sees the change, includes it in Apply
- [ ] Apply flow calls BOTH `apply_to_daemon` (or `save_config` fallback) AND `set_background` — both effects per click
- [ ] `npm run check` (svelte-check) reports 0 errors, 0 warnings
- [ ] `npm run build` succeeds
- [ ] No `console.log` debug statements left behind

**Quality items (non-blocking):**
- [ ] BgGallery loading/error states have visible UI (not silent failure)
- [ ] Active tile has visible selected styling (border/highlight)
- [ ] Layout picker and BgGallery panels visually balanced in the window
- [ ] Thumbnail rendering deferred to a follow-up if not yet wired (acceptable for V1)

### Task 19: Milestone — GUI complete

**Present to user:**
- BgGallery panel renders alongside the layout picker.
- Selecting a bg + clicking Apply updates the daemon (confirmed via `busctl monitor`).
- Selecting "None" + Apply clears the bg.
- Switching layouts after a bg is set: bg persists.

**Wait for user response before proceeding to Phase 4.**

---

## Phase 4: Hardware Verification

### Task 20: End-to-end hardware verification [DO-CONFIRM]

**Implement:**
1. Stop daemon: `systemctl --user stop thermalwriter`.
2. Rebuild + reinstall: `cargo install --path . --force --bin thermalwriter`.
3. Restart daemon: `systemctl --user start thermalwriter`.
4. Launch GUI: `cd gui && npm run tauri dev`.
5. Test scenarios on actual LCD:
   - (a) Pick a seeded bg + the seeded `neon-dash-v2.svg` (the new transparent-canvas version, after copying the updated seed: `cp layouts/svg/neon-dash-v2.svg ~/.config/thermalwriter/layouts/svg/`). Verify bg shows through behind panels.
   - (b) Switch to `arc-gauge.svg`. Verify bg persists, arc-gauge renders on top.
   - (c) Click "None". Verify bg goes away. The seeded layout's transparent canvas means the LCD will show whatever the SvgRenderer's clear color is (likely black). That's the expected "no bg" state.
   - (d) Drop a custom PNG into `~/.config/thermalwriter/backgrounds/` (e.g., a Gemini-generated one), verify it appears in the gallery on focus, select it, Apply.
   - (e) Restart daemon: `systemctl --user restart thermalwriter`. Verify the active bg is restored from config (persistence check).
   - (f) Stop daemon, click Apply in GUI. Verify graceful "Saved — daemon not running" status (the fallback the prior plan implemented). Restart daemon, verify the bg is loaded on startup.

**Confirm checklist:**
- [ ] All 6 scenarios pass on real hardware
- [ ] No journal warnings during normal operation (`journalctl --user -u thermalwriter -f`)
- [ ] User's custom 2 MB inline-bg layout STILL works if they haven't migrated (sanity check — backwards compat)
- [ ] `cargo test --workspace` green on the merged branch
- [ ] No console errors in the GUI's dev console

### Task 21: Final milestone — feature shipped

**Present to user:**
- All 6 hardware scenarios passed.
- Backgrounds work end-to-end: gallery → select → Apply → live LCD.
- Persistence works across daemon restarts.
- Existing layouts still work.
- File the future-enhancement GH issues from the design doc:
  1. Per-layout background overrides
  2. Gemini Nano Banana in-GUI generation
  3. Browse-anywhere file picker
  4. `bg_opacity` knob
  5. Background effects (blur, tint overlay)

**Wait for user response. This is the final milestone.**
