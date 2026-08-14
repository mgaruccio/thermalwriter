# Layout engine status

Pickup note for standardizing SVG layout flow, then measuring performance.
Visual polish of individual cards is out of scope here.

Related: [#99](https://github.com/mgaruccio/thermalwriter/issues/99),
`skills/designing-layouts/`, `docs/profiling.md`.

## Current answer

**No. Auto-grid is not used for all layouts.**

What exists today is a small 1-D primitive, used in **one** place:

| Piece | Where | Status |
|---|---|---|
| `stack()` | `src/render/components/stack.rs` | Landed (`0c02f48`). Fixed item extent; leftover → equal inter-item gap (`space-between`). Cards do not stretch. |
| `token_card` | `responsive_tokens()` in `src/render/svg.rs` | 172 on a 480 short axis (Grafana auto-grid “short” row is ~168). |
| Caller | `layouts/svg/neon-dash-v2.svg` **portrait branch only** | Six striped cards via `stack(count=6, item=token_card, …)`. |
| Square v2 | same file, `{% if is_square %}` | Still a hand-placed 480 artboard (`x=16`, `height=172`, chip row at `y=384`). |
| Wide v2 | same file, `{% else %}` | Still a hand-placed 2-column + chip strip. |
| Every other shipped layout | `layouts/svg/*.svg`, HTML, Blitz | Do **not** call `stack()`. Many user copies under `~/.config/thermalwriter/layouts/` are still fixed `480×480`. |

`seed_layout_dir` never overwrites existing user files, so a daemon upgrade does not install engine changes into an already-seeded layout.

## Prior art we are following

Not a Grafana clone. Two ideas only:

1. **Grafana dashboard layout** — panels live in *tracks* (row height, column count, `gridPos` in grid units), then map onto the viewport. Auto grid can fit panels; named row heights include a short ~168px row.
   - https://grafana.com/docs/grafana/latest/visualizations/dashboards/build-dashboards/create-dashboard/
   - https://grafana.com/docs/grafana/latest/visualizations/dashboards/build-dashboards/view-dashboard-json-model/
2. **CSS Box Alignment `space-between`** — leftover space goes *between* items; first/last sit on the content edges. Children stay fixed size (`flex: none`), not stretched.
   - https://www.w3.org/TR/css-align-3/

Grafana’s “fill screen” *grows* rows. We rejected that: leftover becomes **gap**, not taller cards.

## What “standardized” means

A layout author should declare **modules + a composition**, not pixel tables per resolution.

Allowed composition forks (rare):

- **Column** if `height >= width` (square and portrait share this)
- **Row / 2-column** if `width > height` (landscape / wide / ultrawide)

Not allowed as the long-term pattern:

- `y = 384` / `card_h = 172` literals
- Center-padding a 480 artboard (`translate((width-height)/2) scale(height/480)`)
- A third coordinate system for every new native size
- A new layout language, CSS parser, or `foreignObject`→Taffy path for SVG

Engine work still missing:

- **`stack_fit`** — how many `token_card`s fit in a span at `gap_min` (omit/overflow policy).
- **`hstack`** — same primitive on the other axis (wide 2-column).
- **Column recipe shared by square + portrait** — extra cards appear when `stack_fit` > 2 instead of a separate portrait stylesheet.
- **Chip/row helper** — the 80px RAM/VRAM/FPS strip as a second track size (`token_chip`), not magic `140×80`.
- **Docs in the skill** — `SKILL.md` still tells authors to branch on every `is_*` flag; point them at `stack()` + tokens first.

## Pickup sequence

### 1. Standardize the engine (this doc’s job)

1. Keep `stack()` / `token_card` as the only positioning API for new or migrated SVG layouts.
2. Add `stack_fit` + overflow policy (start-align, never negative gap, never shrink cards).
3. Add `hstack` (or `dir=` on `stack`) and migrate v2 **wide** off literal `col_w` / `y0`.
4. Collapse v2 **square + portrait** onto one column recipe; portrait just shows more modules.
5. Migrate `neon-dash.svg`, `arc-gauge.svg`, `cyber-grid.svg` the same way. Leave experimental `test-*` / Blitz HTML for a later pass.
6. Decide an upgrade path for seeded `~/.config/thermalwriter/layouts/` (overwrite flag vs. documented copy). Until then, copy the repo file when testing.

Definition of done for this phase: `preview_layout --matrix` plus `--size 480x480`, `480x1280`, `1280x480` shows filled canvases from **one** v2 source file; Criterion/layout tests lock `stack()` math.

### 2. Then performance regression

Do this **after** the engine API is stable so we are not rebasing benches onto a moving primitive.

Use the existing two-layer harness in `docs/profiling.md` — do not invent a third profiler.

1. **Save a Criterion baseline on current `master`** before further layout-engine work (`docs/profiling.md`, “Save a Criterion baseline before changing code”).
2. Add or extend render benches for the **same layout at multiple canvases** (480×480, 480×1280, 1280×480) so a flow change cannot hide a portrait-only regression.
3. After each engine/layout migration, `cargo bench` against that baseline. Whole-daemon `scripts/profile.sh neon-dash-v2` is smoke / flamegraph only — not the pass/fail gate.
4. Known expensive reference: `arc-gauge` (see profiling notes). Include it once it uses `stack()`, not before.
5. Reject the change if Criterion render/tick stages regress without a documented tradeoff.

## Local hardware note (not the engine)

The Trofeo (`0416:5302`, PM128, native 1280×480) is being driven **portrait** (`rotation = 90` → oriented 480×1280) with v2’s column stack. Card internals still need visual polish; that is separate from flow. Background remains shared `anime.png` (no per-display background yet).
