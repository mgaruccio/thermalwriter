---
date: 2026-03-23
topic: svg-component-system
---

# SVG Component System & Rendering Enhancements

## What We're Building

A composable component library for the SVG rendering pipeline, enabling layout authors (agents and humans) to assemble rich visualizations from reusable building blocks. The system adds:

1. **Sensor history buffer** — per-layout configurable time-series storage, decoupled from render tick rate
2. **Component library** — Tera functions backed by Rust that emit SVG fragments (graphs, btop-style visualizations, backgrounds)
3. **Background system** — static raster images, procedural SVG patterns, animated GIF/video backgrounds with host-side compositing
4. **Theme palette** — abstract color palette with pluggable source adapters, injected into Tera context
5. **Extended sensors** — per-core CPU metrics, per-CCD temps, network throughput

## Why This Approach

We evaluated three composition strategies:

- **Approach A (Tera Includes):** Simple but no structured metadata for a future app to work with.
- **Approach B (Layout Manifest):** Great for apps but creates a second layout format to maintain.
- **Approach C (Hybrid — chosen):** SVG templates remain the single rendering format. Components are Tera functions backed by Rust. An optional `.meta.toml` sidecar provides structured metadata for future tooling.

Approach C gives us composability now for agents/template authors, with a clean upgrade path to a visual app later. The SVG template is always the source of truth for rendering.

## Key Decisions

### Sensor History Buffer

- **Per-layout configuration** via SVG frontmatter: `{# history: cpu_temp=60s, cpu_util=60s@0.5hz #}`
- **Duration and sample frequency are independent of render tick rate.** History records at sensor poll rate (default 1Hz), queries downsample/interpolate to what the layout requests.
- **Storage:** `HashMap<String, VecDeque<(Instant, f64)>>` — per-metric ring buffers. Non-numeric values silently skipped. Old entries pruned based on max requested duration.
- **Tera injection:** History arrays added to context as `cpu_temp_history`, `cpu_util_history`, etc. — JSON arrays of floats.
- **Query API:** `query(metric, duration, sample_count) → Vec<f64>` returns evenly-spaced samples, interpolated from raw buffer.

### Component Library

- **Location:** `src/render/components/` — `mod.rs`, `graph.rs`, `btop.rs`, `background.rs`, `animation.rs`
- **Registration:** Components implement `tera::Function`, registered on a persistent `Tera` instance (replacing `Tera::one_off()`).
- **History access:** Components receive history arrays as Tera `Value` parameters from context — pure functions of their inputs, no shared mutable state.
- **Defaults:** Every parameter has sensible defaults. Minimal invocation: `{{ graph(metric="cpu_util") }}`

Component signatures:

```svg
{# Graphs #}
{{ graph(metric="cpu_util", x=16, y=200, w=448, h=80, style="line") }}
{{ graph(metric="cpu_temp", x=16, y=200, w=448, h=80, style="area",
         fill="#e9456033", stroke="#e94560") }}

{# btop-style visualizations #}
{{ btop_bars(metric="cpu_util", x=16, y=200, w=448, h=80,
             color="#e94560", per_core=true) }}
{{ btop_net(rx_metric="net_rx", tx_metric="net_tx", x=16, y=300, w=448, h=80,
            rx_color="#53d8fb", tx_color="#e94560") }}
{{ btop_ram(metric="ram_used", total_metric="ram_total", x=16, y=400, w=200, h=60,
            fill="#cc9eff") }}

{# Backgrounds #}
{{ background(source="carbon-fiber.png") }}
{{ background(pattern="grid", color="#ffffff10", spacing=20) }}
{{ background(source="loop.gif", opacity=0.3) }}
{{ background(source="ambient.mp4") }}
```

### btop-Style Visualizations

**CPU/GPU Usage Bars (`btop_bars`):** Grid of `<rect>` elements — columns are time samples, rows are cores. Cell color/opacity mapped to utilization. Can group by CCD.

**Network Bidirectional Graph (`btop_net`):** Two mirrored area `<polygon>` elements sharing a horizontal center axis. RX grows up, TX grows down.

**RAM/VRAM Area Chart (`btop_ram`):** Filled `<polygon>` area chart with current-value `<line>` overlay. Total capacity is the Y ceiling.

### Background & Animation System

**Static raster:** Load PNG/JPEG, base64-encode (cached after first load), emit `<image>` element.

**Procedural patterns:** Built-in SVG pattern generators — `grid`, `dots`, `carbon`, `hexgrid`. Emit `<defs><pattern>` + `<rect>` fill. Pure SVG.

**Animated backgrounds (GIF/video):**

```
Source file → AnimationSource (decode) → FrameCache → AnimationClock → base64 inject → <image> tag
```

- GIF: decoded via `image` crate, all frames held in memory.
- Video: decoded via `ffmpeg` subprocess.
- **Eager vs streaming decode:** Default to streaming for clips > 5 seconds. Configurable via frontmatter: `{# animation: fps=15, decode=stream #}` (options: `stream` | `eager`).
- **Tick rate adaptation:** Animated layouts bump render FPS to match animation framerate (capped at USB throughput ceiling — needs benchmarking).
- **Sensor polling stays decoupled:** When render rate increases for animation, sensor poll rate stays at configured interval (default 1Hz). Tick loop reuses cached sensor data between polls.

**Asset resolution:** Relative to layout file directory → shared `assets/backgrounds/` → global theme background.

### Theme Palette

```rust
pub struct ThemePalette {
    pub primary: String,      // main accent
    pub secondary: String,    // second accent
    pub accent: String,       // highlight
    pub background: String,   // page bg
    pub surface: String,      // card bg
    pub text: String,         // primary text
    pub text_dim: String,     // labels
    pub success: String,      // good values
    pub warning: String,      // caution
    pub critical: String,     // danger
}
```

- **Tera injection:** Flattened as `theme_primary`, `theme_secondary`, etc.
- **Source adapters:** `trait ThemeSource { fn load(&self) -> Result<ThemePalette>; }` — implementations for `DefaultThemeSource` (neon-dash palette), `ManualThemeSource` (config.toml colors). KDE, pywal, terminal adapters added later.
- **Global background override:** `[theme] background_image = "path"` in config.toml.
- **Config:**
  ```toml
  [theme]
  source = "default"
  background_image = "optional/path.png"

  [theme.manual]
  primary = "#e94560"
  secondary = "#53d8fb"
  ```

### Extended Sensors

**Per-core (from sysinfo + hwmon/k10temp):**
- `cpu_c0_util` through `cpu_cN_util` — utilization %
- `cpu_c0_temp` through `cpu_cN_temp` — temperature °C
- `cpu_c0_freq` through `cpu_cN_freq` — frequency MHz

**Per-CCD (AMD, from k10temp + kernel topology):**
- `cpu_ccd0_temp`, `cpu_ccd1_temp` — CCD temperatures (Tccd1, Tccd2)
- `cpu_ccd0_util`, `cpu_ccd1_util` — aggregate utilization per CCD (computed from per-core data via `/sys/devices/system/cpu/cpuN/topology/die_id`)

**Network (from sysinfo):**
- `net_rx` — receive bytes/sec
- `net_tx` — transmit bytes/sec

### Value-Based Text Coloring

Discrete thresholds via existing Tera `{% if %}` conditionals. No new engine work needed — the design skill documents the pattern:

```svg
{% if cpu_temp and cpu_temp | int > 80 %}
  <text fill="{{ theme_critical }}">{{ cpu_temp }}°C</text>
{% elif cpu_temp and cpu_temp | int > 60 %}
  <text fill="{{ theme_warning }}">{{ cpu_temp }}°C</text>
{% else %}
  <text fill="{{ theme_primary }}">{{ cpu_temp | default(value="--") }}°C</text>
{% endif %}
```

## Composability Rules

These are the binding contracts for how components interact. Must be documented in both the project CLAUDE.md and the designing-layouts skill.

1. **Position-independent components.** Every component takes `x, y, w, h` and renders within that bounding box. No component assumes knowledge of its position on the canvas.

2. **Theme-aware defaults.** Every component defaults to `theme_*` variables for colors, overridable with explicit hex values. A layout using all defaults inherits the active theme automatically.

3. **Opt-in history.** Only metrics declared in `{# history: ... #}` frontmatter get buffered. Components needing history for an undeclared metric render empty/gracefully degrade.

4. **Purely additive components.** Components emit SVG elements. They do not modify the canvas or other components. Compositing (opacity, layering, clipping) is the layout author's responsibility via standard SVG attributes.

5. **Single background rule.** At most one background per layout (last `{{ background() }}` call wins). Global theme `background_image` overrides per-layout background when set.

6. **Document-order stacking.** Layering follows SVG document order: background → graphs/visualizations → panels → text. Layout authors control z-order by element placement in the template.

7. **Graceful degradation.** Missing sensor data → `default(value="--")`. Missing history → empty graph. Missing theme → built-in default palette. Missing background asset → transparent. No component should cause a render failure.

8. **Sensor polling independence.** Render tick rate and sensor poll rate are independent. Animation-driven tick rate increases do not increase sensor reads.

## Open Questions

- **USB FPS ceiling:** Need to benchmark max sustainable framerate on actual hardware before committing to animation framerate targets. The Windows trcc app caps at 16 FPS.
- **Video decode memory budget:** Streaming decode for clips > 5s is the default, but the right threshold may shift after benchmarking.
- **Per-core sensor key naming:** Using `cpu_c0_util` pattern — confirm this doesn't collide with anything in existing layouts.
- **CCD topology detection:** Need to verify `/sys/devices/system/cpu/cpuN/topology/die_id` works reliably on the 9950X3D with CachyOS kernel.

## Next Steps

→ writing-plans skill for implementation plan with task breakdown, dependencies, and test strategy
