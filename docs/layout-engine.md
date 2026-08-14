# Layout engine

> **Current destination (2026-08-14):** New layouts use the typed, bounded `.layout.toml` document and the shared module composer. The Config GUI and daemon consume the same Rust document, validator, solver, module emitters, and native preview renderer.
> **Breaking transition:** Layout Studio accepts typed `.layout.toml` documents only. Existing SVG/Tera and HTML source layouts are unsupported by the composer and left untouched; they are not imported or converted.

Read the complete [`.layout.toml` authoring reference](../skills/designing-layouts/references/layout-toml.md) for the schema, module catalog, profile recipes, curved-surface policy, diagnostics, CLI loop, visual checklist, and typed-module extension path. The repository skill banner points to the same reference for agents and layout authors.

## Owner path

Open Thermalwriter Config → **Compose**, choose **Neon Composer**, edit the ordered typed modules, and select **Square**, **Portrait**, **Wide**, or **Curved** in **Preview profile**. The [GUI guide](gui.md) covers the online/offline save and activation path plus the distinct **Copy Error**, **Copy preview image**, and **Copy Design Context** handoffs.

The `2400x1080` Curved profile is a conservative readable-zone/protected-bridge topology guide, not calibrated optical warp. Metric, sparkline, and text modules stay local; bridge spanning is an explicit media capability and document choice. For agent-assisted or paste-only authoring, use the [layout-design skill](../skills/designing-layouts/SKILL.md) and [copyable bootstrap prompt](../skills/designing-layouts/references/bootstrap-prompt.md).

Related: [#99](https://github.com/mgaruccio/thermalwriter/issues/99), [#100](https://github.com/mgaruccio/thermalwriter/issues/100), [#101](https://github.com/mgaruccio/thermalwriter/issues/101), [#102](https://github.com/mgaruccio/thermalwriter/issues/102), [#103](https://github.com/mgaruccio/thermalwriter/issues/103), `docs/profiling.md`.

## What the typed path provides

- Four bounded module kinds: `metric`, `sparkline`, `text`, and `media`.
- Versioned TOML with `deny_unknown_fields`; no coordinates, CSS, SVG embedding, or renderer language.
- Deterministic `column`, `two-column`, and explicit curved `zoned-panorama` recipes.
- Native square (`480x480`), portrait (`480x1280`), wide (`1280x480`), and explicit Thermalright curved (`2400x1080`) profiles.
- Stable diagnostics with file/line/column, profile, module, property, reason, and fix fields.
- One renderer path for GUI previews, the CLI preview example, and daemon frames.

## GUI connection

The Config GUI's Layout Studio starts from the shipped `neon-composer` preset, lets an owner add/reorder/bind typed modules, previews the selected native profile, displays topology and diagnostics, and saves or activates the composition. Its capability metadata mirrors the Rust document instead of exposing arbitrary styles. Save uses a document fingerprint so an external edit is reported rather than overwritten.

The shared Tauri boundary includes `load_layout_preset`, `load_layout_document`, `validate_layout_document`, `preview_layout_document`, `copy_layout_design_context`, `save_layout_document`, and `apply_layout_document`. See the authoring reference for the public fields and the paste-only hand-off artifacts.

## Real CLI path

The preview example validates every requested target before writing output. The required flagship matrix is:

```bash
cargo run --example preview_layout -- --matrix layouts/neon-composer.layout.toml
```

The command prints four native PNG paths under `target/preview` by default. Use `--profile` for one target, `--format json` for machine-readable diagnostics, and `--output-dir` for an iteration directory. Generate → validate → preview matrix → inspect images → iterate is the supported authoring loop.

## Boundaries

The typed path is intentionally bounded. It does not add arbitrary renderer code, CSS/SVG embedding, a plugin ABI, undocumented fields, full legacy migration, or 3D calibration. Existing SVG/HTML sources remain separate legacy layouts and are left untouched; they are not the destination for new layout work.

For the washed-out LCD visual discipline and the legacy-layout boundary, start at [`skills/designing-layouts/SKILL.md`](../skills/designing-layouts/SKILL.md).
