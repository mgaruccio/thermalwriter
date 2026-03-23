# Neon Dash v2 — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use forge:executing-plans to implement this plan task-by-task.

**Goal:** Create a new default SVG layout that upgrades neon-dash with sparkline temperature graphs, a RAM area chart, threshold coloring, and full theme-awareness.

**Architecture:** Single SVG template using the existing component library (graph, btop_ram). No new Rust code — layout authoring, config wiring, and skill documentation.

**Tech Stack:** SVG + Tera templates, existing component functions (graph, btop_ram), existing sensor keys

**Required Skills:**
- `designing-layouts`: Invoke before Task 1 — covers LCD gotchas, color system, typography scale, layout arithmetic, preview workflow

## Context for Executor

### Key Files
- `layouts/svg/neon-dash.svg` — Current default layout to upgrade. 50 lines. Two hero panels (CPU/GPU) + bottom stats row (RAM/VRAM/FPS). Hardcoded colors, no components, no history.
- `layouts/svg/component-showcase.svg` — Reference for component usage syntax. Shows correct Tera variable passing (e.g., `color=theme_primary` not `color="{{ theme_primary }}"`), frontmatter syntax, and `{% set %}` for alpha-suffixed colors.
- `src/config.rs:25-33` — `DisplayConfig::default()` has `default_layout: "svg/neon-dash.svg"`. Must change to point to new layout.
- `src/config.rs:94-130` — `builtin_layouts` module: `include_str!` constants + `seed_layout_dir()` function. Must add the new layout here.
- `skills/designing-layouts/SKILL.md` — Layout authoring skill to update with hero+sparkline pattern.
- `skills/designing-layouts/references/components.md` — Component catalog with full function signatures. Reference for correct arg names.

### Research Findings

**Component usage patterns (from component-showcase.svg):**
- Frontmatter: `{# history: cpu_temp=60s, gpu_temp=60s, ram_used=120s #}` — must be on first line
- Graph area sparkline: `{{ graph(data=cpu_temp_history, x=16, y=100, w=448, h=88, style="area", fill="#e9456022", stroke="#e9456066", stroke_width=1) }}`
- btop_ram: `{% set color_alpha = theme_accent ~ "55" %}` then `{{ btop_ram(data=ram_used_history, total=64.0, x=16, y=392, w=200, h=60, fill=color_alpha) }}`
- Theme vars passed directly (unquoted): `color=theme_primary` NOT `color="{{ theme_primary }}"`
- Text threshold coloring: `{% if cpu_temp and cpu_temp | int >= 80 %}...{% elif %}...{% else %}...{% endif %}`

**Layout arithmetic for neon-dash structure (480x480, 16px padding):**
- Content area: 480 - 32 = 448px wide, 448px tall
- CPU panel: y=16, h=172
- Gap: 12px
- GPU panel: y=200, h=172
- Gap: 12px
- Bottom row: y=384, h=80
- Total: 16 + 172 + 12 + 172 + 12 + 80 + 16 = 480 ✓

**LCD hardware gotchas:**
- Min label size 14px, min color brightness #888888 (matches `theme_text_dim` default)
- Sparkline fill opacity should start at ~20% alpha (hex suffix `33`), adjust on hardware if too subtle
- `font-family="DejaVu Sans Mono, monospace"` is the only reliably available font

**Gradient definitions for themed panels:**
- Current neon-dash uses hardcoded `linearGradient` for panel backgrounds and accent text
- For theme-awareness: use `{{ theme_surface }}` as flat panel fill (gradients with Tera vars in `<stop>` elements work but are verbose)
- Or keep gradients with hardcoded near-blacks for panel bg (`#151520` → `#0e0e18`) since these are structural, not thematic

### Relevant Patterns
- `layouts/svg/neon-dash.svg` — Base structure to preserve (two hero panels + bottom row)
- `layouts/svg/component-showcase.svg:13-16` — Graph sparkline behind panel with fade overlay pattern
- `layouts/svg/component-showcase.svg:20-26` — Threshold coloring pattern with Tera conditionals
- `layouts/svg/component-showcase.svg:53-55` — btop_ram with alpha-suffixed theme color via `{% set %}`

## Execution Architecture

**Team:** 1 dev, 1 spec reviewer, 1 quality reviewer
**Task dependencies:** All sequential — layout → config → skill → milestone
**Phases:**
- Phase 1: Tasks 1-2 — Create layout, wire config
- Phase 2: Task 3 — Update skill
**Milestones:**
- After Task 2 (layout created + config wired): render on hardware, visual review
- After Task 3 (final): skill updated, all done

---

## Phase 1: Layout and Config

### Task 1: Create neon-dash-v2 SVG layout [READ-DO]

**Files:**
- Create: `layouts/svg/neon-dash-v2.svg`

> **Before starting:** Invoke the `designing-layouts` skill. Follow its preview workflow — render after every major change, read the PNG.

**Step 1: Create the base layout with frontmatter and background**

Create `layouts/svg/neon-dash-v2.svg` with:
```xml
{# history: cpu_temp=60s, gpu_temp=60s, ram_used=120s #}
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 480 480" width="480" height="480">
  <defs>
    <linearGradient id="panelGrad" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#151520"/>
      <stop offset="100%" stop-color="#0e0e18"/>
    </linearGradient>
  </defs>

  <!-- Background -->
  <rect width="480" height="480" fill="{{ theme_background }}"/>

  <!-- CPU Panel background -->
  <rect x="16" y="16" width="448" height="172" rx="8" fill="url(#panelGrad)"/>

  <!-- CPU temp sparkline (behind panel text) -->
  {% set cpu_spark_fill = theme_primary ~ "22" %}
  {% set cpu_spark_stroke = theme_primary ~ "66" %}
  {{ graph(data=cpu_temp_history, x=16, y=100, w=448, h=88,
           style="area", fill=cpu_spark_fill, stroke=cpu_spark_stroke, stroke_width=1) }}

  <!-- CPU accent bar -->
  <rect x="16" y="16" width="4" height="172" rx="2" fill="{{ theme_primary }}"/>

  <!-- CPU label -->
  <text x="36" y="46" font-family="DejaVu Sans Mono, monospace" font-size="14" fill="{{ theme_text_dim }}">CPU</text>

  <!-- CPU temp hero (threshold colored) -->
  {% if cpu_temp and cpu_temp | int >= 80 %}
    <text x="36" y="120" font-family="DejaVu Sans Mono, monospace" font-size="64" fill="{{ theme_critical }}" font-weight="bold">{{ cpu_temp }}°C</text>
  {% elif cpu_temp and cpu_temp | int >= 60 %}
    <text x="36" y="120" font-family="DejaVu Sans Mono, monospace" font-size="64" fill="{{ theme_warning }}" font-weight="bold">{{ cpu_temp }}°C</text>
  {% else %}
    <text x="36" y="120" font-family="DejaVu Sans Mono, monospace" font-size="64" fill="{{ theme_primary }}" font-weight="bold">{{ cpu_temp | default(value="--") }}°C</text>
  {% endif %}

  <!-- CPU load -->
  <text x="36" y="168" font-family="DejaVu Sans Mono, monospace" font-size="22" fill="{{ theme_primary }}" opacity="0.7">{{ cpu_util | default(value="--") }}% LOAD</text>

  <!-- CPU power (right side) -->
  <text x="444" y="110" font-family="DejaVu Sans Mono, monospace" font-size="52" fill="{{ theme_primary }}" text-anchor="end" opacity="0.7">{{ cpu_power | default(value="--") }}W</text>

  <!-- GPU Panel background -->
  <rect x="16" y="200" width="448" height="172" rx="8" fill="url(#panelGrad)"/>

  <!-- GPU temp sparkline (behind panel text) -->
  {% set gpu_spark_fill = theme_secondary ~ "22" %}
  {% set gpu_spark_stroke = theme_secondary ~ "66" %}
  {{ graph(data=gpu_temp_history, x=16, y=284, w=448, h=88,
           style="area", fill=gpu_spark_fill, stroke=gpu_spark_stroke, stroke_width=1) }}

  <!-- GPU accent bar -->
  <rect x="16" y="200" width="4" height="172" rx="2" fill="{{ theme_secondary }}"/>

  <!-- GPU label -->
  <text x="36" y="230" font-family="DejaVu Sans Mono, monospace" font-size="14" fill="{{ theme_text_dim }}">GPU</text>

  <!-- GPU temp hero (threshold colored) -->
  {% if gpu_temp and gpu_temp | int >= 80 %}
    <text x="36" y="304" font-family="DejaVu Sans Mono, monospace" font-size="64" fill="{{ theme_critical }}" font-weight="bold">{{ gpu_temp }}°C</text>
  {% elif gpu_temp and gpu_temp | int >= 60 %}
    <text x="36" y="304" font-family="DejaVu Sans Mono, monospace" font-size="64" fill="{{ theme_warning }}" font-weight="bold">{{ gpu_temp }}°C</text>
  {% else %}
    <text x="36" y="304" font-family="DejaVu Sans Mono, monospace" font-size="64" fill="{{ theme_secondary }}" font-weight="bold">{{ gpu_temp | default(value="--") }}°C</text>
  {% endif %}

  <!-- GPU load -->
  <text x="36" y="352" font-family="DejaVu Sans Mono, monospace" font-size="22" fill="{{ theme_secondary }}" opacity="0.7">{{ gpu_util | default(value="--") }}% LOAD</text>

  <!-- GPU power (right side) -->
  <text x="444" y="294" font-family="DejaVu Sans Mono, monospace" font-size="52" fill="{{ theme_secondary }}" text-anchor="end" opacity="0.7">{{ gpu_power | default(value="--") }}W</text>

  <!-- Bottom Stats Row -->

  <!-- RAM card with area chart -->
  <rect x="16" y="384" width="140" height="80" rx="6" fill="url(#panelGrad)"/>
  {% set ram_fill = theme_accent ~ "44" %}
  {{ btop_ram(data=ram_used_history, total=64.0, x=16, y=410, w=140, h=54, fill=ram_fill) }}
  <text x="86" y="420" font-family="DejaVu Sans Mono, monospace" font-size="22" fill="{{ theme_accent }}" text-anchor="middle">{{ ram_used | default(value="--") }}G</text>
  <text x="86" y="452" font-family="DejaVu Sans Mono, monospace" font-size="16" fill="{{ theme_text_dim }}" text-anchor="middle">RAM</text>

  <!-- VRAM card -->
  <rect x="170" y="384" width="140" height="80" rx="6" fill="url(#panelGrad)"/>
  <text x="240" y="420" font-family="DejaVu Sans Mono, monospace" font-size="22" fill="{{ theme_accent }}" text-anchor="middle">{{ vram_used | default(value="--") }}G</text>
  <text x="240" y="452" font-family="DejaVu Sans Mono, monospace" font-size="16" fill="{{ theme_text_dim }}" text-anchor="middle">VRAM</text>

  <!-- FPS card -->
  <rect x="324" y="384" width="140" height="80" rx="6" fill="url(#panelGrad)"/>
  <text x="394" y="420" font-family="DejaVu Sans Mono, monospace" font-size="26" fill="{{ theme_accent }}" text-anchor="middle">{{ fps | default(value="--") }}</text>
  <text x="394" y="452" font-family="DejaVu Sans Mono, monospace" font-size="16" fill="{{ theme_text_dim }}" text-anchor="middle">FPS</text>

</svg>
```

**Step 2: Preview the layout**

Run: `cargo run --example preview_layout layouts/svg/neon-dash-v2.svg`
Expected: PNG saved to `/tmp/thermalwriter_neon-dash-v2.png`

Read the PNG and verify:
- CPU and GPU panels render with hero temps and sparkline graphs visible behind them
- Bottom row has RAM area chart in left card
- Theme colors used throughout (red CPU, cyan GPU, teal accents)
- Labels visible (not washed out — at least #888888)
- No SVG rendering errors in console output

**Step 3: Push to hardware**

Run:
```bash
systemctl --user stop thermalwriter
cargo run --example render_layout layouts/svg/neon-dash-v2.svg 15
systemctl --user start thermalwriter
```

Verify on the physical LCD:
- Text is readable
- Sparkline opacity is right (not too subtle, not too dominant)
- Colors look correct on hardware (LCD backlight can wash out dim colors)

If sparkline is too subtle, increase alpha from `22` to `33` or `44`. If too dominant, reduce to `18`.

**Step 4: Commit the layout**

```bash
git add layouts/svg/neon-dash-v2.svg
git commit -m "feat: add neon-dash-v2 layout with sparkline graphs and theme support"
```

---

### Task 2: Wire neon-dash-v2 as default layout [DO-CONFIRM]

**Files:**
- Modify: `src/config.rs:29` — change default_layout
- Modify: `src/config.rs:101-116` — add builtin layout constant and seed entry

**Implement:**

1. In `src/config.rs:29`, change:
   ```rust
   default_layout: "svg/neon-dash-v2.svg".to_string(),
   ```

2. In `src/config.rs:101-103`, add after the `SVG_CYBER_GRID` constant:
   ```rust
   pub const SVG_NEON_DASH_V2: &str = include_str!("../layouts/svg/neon-dash-v2.svg");
   ```

3. In `src/config.rs:109-116`, add to the `layouts` array:
   ```rust
   ("svg/neon-dash-v2.svg", SVG_NEON_DASH_V2),
   ```

4. Keep the old neon-dash.svg in the builtin list (don't remove — users may have customized it).

**Confirm checklist:**
- [ ] `DisplayConfig::default().default_layout` == `"svg/neon-dash-v2.svg"` — check `src/config.rs:29`
- [ ] `SVG_NEON_DASH_V2` constant includes the new layout — check `src/config.rs`
- [ ] `seed_layout_dir` includes `("svg/neon-dash-v2.svg", SVG_NEON_DASH_V2)` — check the array
- [ ] Old `SVG_NEON_DASH` constant and seed entry still exist (not removed)
- [ ] `cargo test` passes — existing config tests should still work since they use `Config::default()` which now points to v2
- [ ] `cargo build` succeeds
- [ ] Committed with clear message

---

### Task 3: Review Tasks 1-2

**Trigger:** Both reviewers start simultaneously when Tasks 1-2 complete.

**Killer items (blocking):**
- [ ] `neon-dash-v2.svg` renders without errors: `cargo run --example preview_layout layouts/svg/neon-dash-v2.svg` exits 0 and produces PNG
- [ ] Frontmatter declares `cpu_temp`, `gpu_temp`, `ram_used` history — check first line of SVG
- [ ] All sensor values use `| default(value="--")` fallback — grep for `{{ cpu_temp` / `{{ gpu_temp` / etc. and confirm each has default or is inside a `{% if %}` guard
- [ ] Theme variable passing uses unquoted vars (e.g., `fill=cpu_spark_fill`) not `fill="{{ ... }}"` — grep for `"{{` in component calls (should find none)
- [ ] `DisplayConfig::default().default_layout` is `"svg/neon-dash-v2.svg"` — check `src/config.rs:29`
- [ ] Old neon-dash.svg still exists in builtin layouts — check `src/config.rs` for `SVG_NEON_DASH` constant

**Quality items (non-blocking):**
- [ ] No text element uses fill color below #888888 brightness
- [ ] Sparkline positioned within its parent panel bounding box (not overflowing)
- [ ] Panel gradient defs still work (SVG `<defs>` present with panelGrad)

---

### Task 4: Milestone — Layout created and wired as default

**Present to user:**
- Rendered PNG of neon-dash-v2 on hardware
- Comparison with old neon-dash (structure preserved, sparklines added, theme-aware)
- Config default changed
- Test results

**Wait for user response before proceeding to Phase 2.**

---

## Phase 2: Skill Update

### Task 5: Update designing-layouts skill with hero+sparkline pattern [DO-CONFIRM]

**Files:**
- Modify: `skills/designing-layouts/SKILL.md` — add hero+sparkline composition pattern
- Modify: `skills/designing-layouts/references/layout-patterns.md` — add neon-dash-v2 as reference example (if file exists; create if not)

**Implement:**

Add a "Hero + Sparkline" section to the layout patterns documentation showing:
- The technique: area graph component positioned behind a hero panel, low-opacity fill, text rendered on top
- The alpha suffix trick: `{% set color_alpha = theme_primary ~ "22" %}` for Tera string concatenation
- Recommended opacity ranges: `18`-`33` for subtle background, `44`-`66` for prominent
- Example: simplified extract from neon-dash-v2 showing a CPU panel with sparkline
- Note: sparkline bounding box should be clipped to the panel area (position within panel rect bounds)

**Confirm checklist:**
- [ ] "Hero + Sparkline" pattern documented with code example
- [ ] Alpha suffix technique explained (`theme_primary ~ "22"`)
- [ ] Opacity recommendations for LCD hardware included
- [ ] neon-dash-v2 referenced as the canonical example of this pattern
- [ ] Committed with clear message

---

### Task 6: Review Task 5

**Trigger:** Both reviewers start simultaneously when Task 5 completes.

**Killer items (blocking):**
- [ ] Hero + sparkline pattern example uses correct component syntax (matches `graph()` signature in `references/components.md`)
- [ ] Alpha suffix example uses `{% set %}` + `~` concatenation (not `"{{ }}"` nested in string)
- [ ] Opacity recommendations mention LCD hardware brightness considerations

**Quality items (non-blocking):**
- [ ] Example is self-contained (could be copy-pasted into a new layout and render)
- [ ] Cross-references to components.md for full graph() signature

---

### Task 7: Milestone — Final

**Present to user:**
- neon-dash-v2 as default layout, running on hardware
- Skill updated with hero+sparkline pattern
- Full test suite results
- Summary of what changed

**This is the final milestone. Wait for user approval.**
