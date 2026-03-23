---
date: 2026-03-23
topic: neon-dash-v2
---

# Neon Dash v2 — Default Layout Upgrade

## What We're Building

A single SVG layout that replaces `svg/neon-dash.svg` as the default daily driver. Keeps the proven two-panel hero structure (CPU temp, GPU temp) but adds sparkline area graphs behind each panel showing 60s of temperature history, and upgrades the bottom stats row with a small RAM area chart. Fully theme-aware via `theme_*` variables. Threshold coloring on temps.

## Why This Approach

The current neon-dash is static text — no graphs, no history, no theme awareness. The new component system enables sparklines and area charts as background decoration without cluttering the layout. We're enhancing what already works rather than redesigning from scratch.

Balanced hero + graphs was chosen over temperature-forward (too sparse) and activity-forward (too busy for a daily driver). The sparklines add visual interest without competing with the hero numbers.

## Key Decisions

- **Sparklines as background decoration**: Low opacity (~20-30% alpha fill) behind panel text. Temp is readable from the hero number; the graph shows trend.
- **History frontmatter**: `{# history: cpu_temp=60s, gpu_temp=60s, ram_used=120s #}` — 60s for temp sparklines, 120s for RAM (slower-changing metric).
- **Threshold coloring**: Both CPU and GPU temps use `theme_success` (< 60°C), `theme_warning` (60-80°C), `theme_critical` (> 80°C) via Tera conditionals.
- **Bottom row RAM area chart**: Small `btop_ram` component. VRAM and FPS stay as simple text values.
- **All colors via theme variables**: `theme_primary` (CPU), `theme_secondary` (GPU), `theme_accent` (FPS), `theme_surface` (panels), `theme_background` (page), `theme_text_dim` (labels).
- **No background pattern/image**: Clean dark background. Sparklines provide visual interest.
- **Replaces neon-dash as default**: Update config default and built-in layout seeding.

## Layout Structure

```
480x480 canvas, 16px outer padding, 10px gaps

CPU Panel (16, 16, 448×172):
  - "CPU" label (theme_text_dim, 14px)
  - cpu_temp hero (64px, threshold-colored)
  - cpu_util "% LOAD" (22px, theme_primary dimmed)
  - cpu_power "W" right-aligned (52px, theme_primary, 0.7 opacity)
  - graph(cpu_temp_history) area sparkline behind panel (fill: theme_primary ~20% alpha)

GPU Panel (16, 200, 448×172):
  - Same structure as CPU, theme_secondary colors
  - graph(gpu_temp_history) area sparkline behind panel

Bottom Row (16, 384, 448×80):
  - RAM card (140px wide): btop_ram area chart + "RAM" label + value
  - VRAM card (140px wide): value + "VRAM" label
  - FPS card (140px wide): value + "FPS" label (theme_accent)
```

## Open Questions

- Exact sparkline opacity — need to test on hardware. Start at 20% alpha, adjust if too subtle or too dominant.

## Next Steps

→ writing-plans skill for implementation plan
