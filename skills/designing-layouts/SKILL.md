> **Authoring destination (2026-08-14):** New work targets the typed `.layout.toml` composer in `src/layout_engine/`. Read [the `.layout.toml` authoring reference](./references/layout-toml.md) for the real schema, four module kinds, profile recipes, curved policies, diagnostics, CLI loop, and extension path. Keep the visual craft below: these are small, often washed-out LCDs. Existing SVG/HTML layouts remain untouched legacy sources.

---
name: designing-layouts
description: Use when creating, modifying, or reviewing Thermalwriter LCD layouts. New authoring uses bounded .layout.toml documents; legacy SVG/HTML maintenance is documented separately.
---

# Designing Layouts for Thermalwriter

## New layout work

Use the typed document path for every new composition:

1. Start from `layouts/neon-composer.layout.toml` or the Neon Composer preset in the Config GUI.
2. Order a small set of `metric`, `sparkline`, `text`, and `media` modules; bind them to the documented catalog keys.
3. Select the profile recipe for the actual native surface. Do not enter coordinates or renderer styles.
4. Run the real matrix preview and open every generated PNG.
5. Fix diagnostics, iterate, and repeat before saving or activating.

The complete contract and exact commands live in [references/layout-toml.md](./references/layout-toml.md). The flagship check is:

```bash
cargo run --example preview_layout -- --matrix layouts/neon-composer.layout.toml
```

The command validates all requested targets before rendering and prints exact native PNG paths. Use `--format json` when an agent needs stable diagnostic fields. A passing command still requires visual inspection.

## The visual goal

**Attractive is more important than informational.** Thermalright panels are compact and can look washed out inside a case. Design for a consumer display such as CAM or iCUE, not a terminal wall: bold hierarchy, a dark tinted background, a few large values, and clear accent colors.

### LCD contrast

- Never use pure black as the only background. Prefer `#08080f`, `#0a0a14`, or the typed engine's default `#080c14`.
- Use a subtly lighter panel (`#12121e`, `#1a1a2e`, or the typed default `#17202c`) to create depth.
- Keep typed foreground colors at or above the engine's `#999999` per-channel readability floor. Labels must remain visible on hardware.
- Use bright accents for hero values, quieter accents for units, and neutral gray for labels. Do not give every string the same brightness.
- Prefer one hero value plus four to six supporting values. Cut a module before shrinking text until it is unreadable.

Suggested metric accents:

| Metric | Accent | Dimmed companion |
|---|---|---|
| CPU temperature/load | `#e94560` | `#c4546e` |
| GPU temperature/load | `#53d8fb` | `#5aabb8` |
| RAM/VRAM | `#cc9eff` | `#bb86fc` |
| FPS/frametime | `#20f5d8` | `#03dac6` |
| Power | `#FFD080` | `#FFB74D` |

### Composition

- Leave breathing room around the hero card and use consistent gaps; dense grids become illegible at native size.
- Keep labels short and values dominant. Units should support the value, not compete with it.
- Use stable IDs and document order intentionally; the solver preserves order across profiles.
- Check square, portrait, wide, and curved output. A composition that looks good on a square may need fewer modules on a wide two-column surface.
- For a curved profile, keep ordinary information in the readable zones. Reserve bridge spanning for intentional media.

## Geometry mental model

The typed solver owns placement. Authors choose a recipe, not a pixel table:

- `column` uses fixed module extents on square and portrait surfaces; leftover space becomes gaps.
- `two-column` uses two fixed-width tracks on wide surfaces and preserves document order.
- `zoned-panorama` uses the explicit Thermalright curved topology: left-readable, protected center bridge, right-readable.
- Cards do not stretch to fill leftover space, shrink below the typed minimum, overlap, or spill through a protected region.

See the authoring reference for the exact profile names, capacities, bridge policy, and module fields. Do not add undeclared style or coordinate keys.

## Legacy layout boundary

SVG/HTML layouts in the repository and user layout directory are legacy sources. They may need maintenance, but they are not the destination for new designs and are not silently converted by the typed engine. If an existing legacy file must be changed, preserve its own format and use its established preview path; do not copy legacy implementation patterns into a new `.layout.toml` document.

## Common mistakes

| Mistake | Fix |
|---|---|
| No visual review | Run the matrix preview and open every PNG before claiming the layout is ready. |
| Low contrast | Use a tinted near-black background, a raised panel, and foreground channels at or above `#999999`. |
| Too many metrics | Keep four to six meaningful values and make the hero treatment larger. |
| Curved content in the bridge | Use local modules for readable zones; opt into bridge spanning only for capable media under the curved profile policy. |
| Invented TOML fields | Check the authoring reference; unknown fields are rejected. |
| Unavailable sensor surprise | Use a documented binding and accept the stable `--` fallback for a missing runtime value. |
| Media path failure | Use a relative image below the approved media directory; avoid `..`, symlinks, and oversized files. |
| Treating a passing render as proof | Inspect the actual native PNG, not only the command exit status. |

## Reference

- [layout-toml.md](./references/layout-toml.md) — Current schema, module catalog, recipes, curved policy, diagnostics, commands, GUI boundary, and extension guide.
