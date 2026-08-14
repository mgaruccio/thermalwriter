# `.layout.toml` authoring reference

This is the authoring contract for the typed Thermalwriter layout engine. A
`.layout.toml` document is a bounded, human-readable composition shared by the
Config GUI and the daemon. It declares an ordered list of typed modules and
named profile recipes; it does **not** contain pixel coordinates, CSS, SVG, or
an embedded renderer language.

New layouts should start here. Existing SVG/HTML layouts remain separate legacy
sources and are not silently rewritten or converted by this engine.

## The authoring loop

Use the same loop whether a person, an integrated coding agent, or a
paste-only chatbot is authoring the document:

1. **Generate** a document from the shipped `neon-composer` preset, or copy the
   preset and edit only the fields documented below.
2. **Validate** the TOML and every selected display profile.
3. **Preview the matrix** at square, portrait, wide, and the explicit curved
   profile.
4. **Inspect every PNG** at native dimensions. Look for hierarchy, clipping,
   missing values, and safe placement around the curved bridge.
5. **Iterate** the TOML, then run validation and the matrix again.

A successful render is not visual proof. Keep the exact PNG paths printed by the
preview command so an agent or a paste-only tool can open or upload the images.
The Config GUI follows the same path through its shared Rust validator,
solver, and renderer.

### Integrated GUI path

The Config GUI's Layout Studio is the preferred authoring tool:

1. Choose the **Neon Composer** preset and give the draft a name.
2. Add, remove, and reorder modules. Document order is solver order.
3. Select a binding and the bounded presentation options in the module
   inspector.
4. Select **Square**, **Portrait**, **Wide**, or **Curved** to preview a native
   surface. Curved previews label readable zones and the protected bridge.
5. Fix diagnostics before saving. **Save layout** writes an atomic document;
   **Save & activate** also asks the daemon to use it.

The GUI exposes the same document fields in
`gui/src/lib/layout/types.ts`. Its `moduleCapabilities` table is the UI
vocabulary for bindings, variants, media fit, opacity, and bridge choices; it
intentionally has no raw-style escape hatch.

### Paste-only and agent path

A disconnected tool can create a file with the schema example below, run the
real preview command, and paste back the diagnostic JSON plus the generated
PNG paths. In the GUI, **Copy Error**, **Copy Preview Image**, and **Copy
Design Context** provide the equivalent hand-off artifacts. Do not invent a
second schema for a chatbot.

## Start from the shipped flagship

The canonical shipped document is
`layouts/neon-composer.layout.toml`. Its complete shape is:

```toml
version = 1
name = "neon-composer"
preset = "neon-composer"

[[modules]]
id = "cpu-temp"
kind = "metric"
binding = "cpu.temperature"
variant = "hero"

[[modules]]
id = "history"
kind = "sparkline"
binding = "cpu.temperature.history"
variant = "neon"

[profiles.square]
recipe = "column"

[profiles.portrait]
recipe = "column"

[profiles.wide]
recipe = "two-column"

[profiles.thermalright-curved-2400x1080]
recipe = "zoned-panorama"
bridge = "media-only"
```

The optional `preset` value identifies the GUI starter; it does not replace
`modules` or `profiles`, and it is not a renderer hook. The current GUI ships
one preset ID, `neon-composer`.

For a schema-only minimal shape, every field below is still a real field and
every value is from the current catalog:

```toml
version = 1
name = "thermal-overview"
[[modules]]
id = "cpu-temp"
kind = "metric"
binding = "cpu.temperature"
variant = "hero"
[profiles.thermalright-curved-2400x1080]
recipe = "zoned-panorama"
bridge = "media-only"
```

Use the shipped preset as the visual baseline even when starting from a small
fragment like this one.

## Document schema

`LayoutDocument` uses `serde(deny_unknown_fields)`. Unknown keys are errors;
there is no supported `style`, coordinate, CSS, SVG, or plugin field.

### Top-level fields

| TOML field | Type | Required | Meaning |
|---|---|---:|---|
| `version` | integer | yes | Document format version. The only supported value is `1`. |
| `name` | string | yes | Human-facing composition name. When saved, it becomes the safe filename stem for `<name>.layout.toml`. |
| `preset` | string | no | Optional starter identity, currently `neon-composer` in the GUI. |
| `modules` | array of tables | yes | Ordered typed modules. Empty arrays parse, but a useful composition normally has at least one module. |
| `profiles` | table of tables | yes | Profile name to recipe policy. Use the documented profile names and fields. |

The parser rejects unsupported versions without rewriting the input. Saving also
normalizes the name as one direct filename component: no path separators,
parent-directory components, control characters, legacy `.svg`/`.html` suffixes,
or trailing dot/space.

### Modules: common fields

Every `[[modules]]` entry has these required fields:

| Field | Type | Meaning |
|---|---|---|
| `id` | string | Stable, non-empty, unique identity used by solver results, GUI selection, and diagnostics. |
| `kind` | enum string | Exactly one of `metric`, `sparkline`, `text`, `media`. |
| `binding` | string | Runtime sensor/history key, or the media catalog/source key. |
| `variant` | string | Curated presentation value for the selected kind. |

Keep IDs stable while iterating so GUI and diagnostic output remain easy to
match. Duplicate or empty IDs fail validation with `TWLAYOUT-E023`.

The runtime binding boundary accepts namespaced keys such as
`cpu.temperature`, `cpu.utilization`, `cpu.power`, `gpu.temperature`,
`gpu.utilization`, `gpu.power`, `gpu.memory.used`, `gpu.memory.total`,
`memory.used`, `memory.total`, `network.receive`, `network.transmit`,
`game.fps`, and `game.frametime`. History keys conventionally end in
`.history`; the flagship uses `cpu.temperature.history`. The GUI derives its
binding picker from the daemon sensor catalog. A temporarily unavailable
binding renders the bounded unavailable value (`--`) rather than crashing.

### `metric`

A live numeric or status card:

```toml
[[modules]]
id = "gpu-temp"
kind = "metric"
binding = "gpu.temperature"
variant = "compact"
```

Supported metric variants are:

- `default` — balanced value card
- `hero` — larger primary value treatment
- `compact` — tighter value treatment
- `status` — status-sized value treatment

Metric modules are local-only on curved surfaces. Thresholds are an internal
emitter capability, not a persisted `.layout.toml` field; do not add a
`threshold` key to a document.

### `sparkline`

A bounded history visualization:

```toml
[[modules]]
id = "cpu-history"
kind = "sparkline"
binding = "cpu.temperature.history"
variant = "area"
```

Supported sparkline variants are:

- `default` — standard accent line
- `line` — line only
- `area` — bounded filled area
- `neon` — brighter, heavier filled treatment
- `muted` — lower-emphasis line

The persisted document currently exposes the binding and variant. Numeric
range configuration is an emitter API, not a document field. Sparklines are
local-only on curved surfaces.

### `text`

A bounded text value selected by a runtime binding:

```toml
[[modules]]
id = "host-status"
kind = "text"
binding = "cpu.temperature"
variant = "status"
```

Supported text variants map to the scene's semantic roles:

`body`, `title`, `label`, `caption`, `value`, `unit`, and `status`.

Text is not an arbitrary literal/template field in the persisted document. It
is bound text with a bounded fallback. Text modules are local-only on curved
surfaces.

### `media`

A local image module is the only initial bridge-capable module. Its required
common fields are followed by these optional fields:

| TOML field | Type | Default | Meaning |
|---|---|---:|---|
| `source` | path string | empty | Relative local image path below the approved media/layout directory. If empty, `binding` is used as the source. |
| `fit` | enum string | `contain` | Either `contain` (show the whole image) or `cover` (fill and crop). |
| `span_bridge` | boolean | `false` | Records the media module's request to span a permitted curved bridge. The profile policy is still required. |
| `opacity` | number | `1.0` | Finite media opacity from `0.7` through `1.0`. |

Example with all media fields:

```toml
[[modules]]
id = "wallpaper"
kind = "media"
binding = "wallpaper.png"
variant = "default"
source = "wallpaper.png"
fit = "cover"
span_bridge = true
opacity = 0.9
```

Use a relative filename below the approved media directory. Do not use `..`,
symlink escapes, or an unbounded external path. Media is decoded through the
bounded media cache; missing, malformed, oversized, or unsafe files produce a
diagnostic instead of an allocation or process crash.

## Profiles, recipes, and geometry

A profile table contains exactly the fields shown here:

```toml
[profiles.square]
recipe = "column"
```

| Profile key | Native surface | Required recipe |
|---|---:|---|
| `square` | `480x480` rectangular | `column` |
| `portrait` | `480x1280` rectangular | `column` |
| `wide` | `1280x480` rectangular | `two-column` |
| `thermalright-curved-2400x1080` | `2400x1080` curved panorama | `zoned-panorama` |

The solver can also resolve the explicit `rectangular` surface ID for the
480x480 target and registered fixture profiles, but the four keys above are the
stable authoring targets. A matching `2400x1080` resolution does **not** imply
curvature; the curved topology must be selected explicitly.

`recipe` is one of the following real enum values:

- `column` — fixed-size modules flow vertically. Square and portrait use the
  same recipe; portrait simply has more vertical capacity.
- `two-column` — fixed-width modules occupy two horizontal tracks in document
  order. An odd final module stays in the first track.
- `zoned-panorama` — places modules into the two readable zones of the
  explicitly selected curved surface.

The solver derives the content inset and fixed card extent from the short axis.
It never accepts author coordinates, stretches cards to consume leftover space,
creates negative gaps, shrinks cards below the typed minimum, overlaps modules,
or silently drops overflow. Leftover room is distributed as a gap between fixed
modules. Capacity failures are validation errors (`TWLAYOUT-E022` for
rectangular recipes and `TWLAYOUT-E027` for a curved local zone).

An explicit rectangular profile recipe must match the aspect class: `column`
for `height >= width`, `two-column` for `width > height`. A mismatch is
`TWLAYOUT-E021`; an unknown recipe is `TWLAYOUT-E025`.

## Curved local and spanning rules

The registered curved profile is intentionally conservative and is a topology
model, not optical calibration or a mesh/perspective correction:

- native canvas: `2400x1080`
- `left-readable`: `x=0..960`, full height
- `center-bridge`: `x=960..1440`, full height, protected
- `right-readable`: `x=1440..2400`, full height

The readable zones are 40% / 20% / 40% of the canvas. Their interiors must not
overlap the bridge; touching the boundary is allowed. The renderer and GUI use
these registered bounds for the solve and preview overlay.

Use this profile table for a curved document:

```toml
[profiles.thermalright-curved-2400x1080]
recipe = "zoned-panorama"
bridge = "media-only"
```

`bridge` is optional and defaults to `local-only`. Its enum values are:

- `local-only` — no module may occupy the protected bridge. This is the safe
  default and the only policy that guarantees a fully local composition.
- `media-only` — the current media module is permitted to span the full
  `2400x1080` canvas when the document requests bridge spanning.
- `explicit-capable` — reserved for modules that advertise an explicit bridge
  capability. In the current catalog its effect is still media-only; metrics,
  sparklines, and text do not gain bridge access.

The spanning decision has two separate parts:

1. the selected curved profile must use a spanning policy (`media-only` or
   `explicit-capable`), and
2. the module must be the bridge-capable `media` kind.

Set `span_bridge = true` when the media module is intentionally requesting a
bridge span; this is the request surfaced by the GUI and design-context output.
The bounded solver's placement authority is the profile policy plus the module
capability, so `bridge = "local-only"` is the only policy that guarantees a
fully local composition. A media request alone is not permission to enter the
bridge, and a policy alone does not make metric, sparkline, or text modules
span.


All ordinary modules remain local. Local modules are assigned round-robin to
left then right readable zones in document order. Each zone gets fixed card
extents and a local capacity; modules never spill through the bridge. If a zone
overflows, validation reports `TWLAYOUT-E027`; the CLI emits no preview PNG and
the GUI returns diagnostics with an empty RGBA frame.
## How the GUI and daemon stay connected

The document is the shared boundary, not a GUI-only export format:

- Rust owns parsing, version checks, validation, solving, typed module
  emission, media containment, rasterization, and atomic persistence under
  `src/layout_engine/`.
- The Config GUI mirrors the document in
  `gui/src/lib/layout/types.ts`. `moduleCapabilities` supplies the bounded
  inspector choices for each module kind; it must not add raw styles.
- GUI preview profiles map to the same native targets: the UI's `curved` choice
  selects backend profile `thermalright-curved-2400x1080`.
- Tauri commands use the shared path: `load_layout_preset`,
  `load_layout_document`, `validate_layout_document`,
  `preview_layout_document`, `copy_layout_design_context`,
  `save_layout_document`, and `apply_layout_document`.
- A preview response carries native `width`, `height`, RGBA pixels,
  `diagnostics`, topology, and a document fingerprint. The GUI labels the
  registered readable/protected regions from that topology (and uses its
  conservative 40/20/40 overlay when richer bounds are not present). Save uses
  the fingerprint to reject an external edit instead of overwriting it silently.

The daemon and preview example both construct `LayoutEngineRenderer`; the
integration matrix test proves that they produce identical pixels for the same
flagship document, profile, and sensor input.

## Diagnostics and correction

Diagnostics are stable data, not text that an agent must scrape. The shared
`LayoutDiagnostic` fields are:

| Field | Meaning |
|---|---|
| `code` | Stable machine-readable code such as `TWLAYOUT-E022`. |
| `severity` | One of `error`, `warning`, or `info`. |
| `message` | Short human summary. |
| `file` | Optional source path. |
| `line`, `column` | Optional one-based TOML source location. |
| `profile` | Optional affected profile name or surface ID. |
| `module_id` | Optional affected typed module ID. |
| `property_path` | Optional field path such as `recipe`, `modules[].id`, or `bridge`. |
| `reason` | Detailed explanation of the failure. |
| `fix` | Suggested correction. |

Human output is standalone and pasteable:

```text
TWLAYOUT-E022 [error] Layout recipe capacity exceeded
Location: layouts/my-layout.layout.toml
Profile: square
Property: modules
Reason: Recipe `column` cannot place modules without shrinking or overlap: ...
Fix: Remove or reorder modules so this surface uses at most ...
```

Use `--format json` with the preview example to retain the same fields for an
integrated tool. Common authoring codes include:

| Code | Meaning |
|---|---|
| `TWLAYOUT-E001` | TOML parse/decode failure. |
| `TWLAYOUT-E002` | Unsupported document version reported by the preview CLI. |
| `TWLAYOUT-E014` | Unsupported persisted property. |
| `TWLAYOUT-E015` | Typed module data/style or emission failure. |
| `TWLAYOUT-E020` | Curved recipe selected for a rectangular surface. |
| `TWLAYOUT-E021` | Recipe does not match rectangular aspect. |
| `TWLAYOUT-E022` | Rectangular capacity or fixed-card fit failure. |
| `TWLAYOUT-E023` | Empty or duplicate module ID. |
| `TWLAYOUT-E024` | Invalid rectangular surface topology. |
| `TWLAYOUT-E025` | Unknown recipe name. |
| `TWLAYOUT-E026` | Unsafe or unknown curved bridge policy. |
| `TWLAYOUT-E027` | Curved local-zone capacity failure. |
| `TWLAYOUT-E028` | Unsupported CLI preview profile. |
| `TWLAYOUT-E030` | Scene backend compilation/rasterization failure. |
| `TWLAYOUT-E031` | Bounded media-cache/decode failure. |
| `TWLAYOUT-E032` | Internal solve/render/native-dimension mismatch. |
| `TWLAYOUT-E040` | Persistence/filesystem failure. |
| `TWLAYOUT-E041` | Stale document fingerprint conflict. |
| `TWLAYOUT-E042` | Unsafe layout name or path. |
| `TWLAYOUT-E043` | Legacy source supplied to typed persistence. |

## Generate, validate, preview, inspect, iterate

### 1. Generate

From the repository root, copy the real preset before editing:

```bash
cp layouts/neon-composer.layout.toml layouts/my-layout.layout.toml
$EDITOR layouts/my-layout.layout.toml
```

Alternatively, use the GUI's Neon Composer starter. Keep the `.layout.toml`
suffix; typed persistence and the CLI use it to distinguish this document path
from legacy layout sources.

### 2. Validate one target

The preview example validates the parsed document and the requested surface
**before** creating output or rendering pixels. There is no separate shell
validator to keep in sync with the renderer. Use a one-target run when fixing
an error:

```bash
cargo run --example preview_layout -- \
  --format json \
  --profile square \
  layouts/my-layout.layout.toml
```

Human output is the default:

```bash
cargo run --example preview_layout -- \
  --profile wide \
  --output-dir target/layout-preview \
  layouts/my-layout.layout.toml
```

Supported document profile names are `square`, `portrait`, `wide`, and
`thermalright-curved-2400x1080`. `--size 480x480`, `--size 480x1280`, and
`--size 1280x480` select the corresponding registered rectangular profiles.
The curved target must be selected by its explicit profile name; dimensions
alone never infer curvature.

### 3. Preview the required matrix

Use the real flagship command when checking the engine or a new authoring
workflow:

```bash
cargo run --example preview_layout -- --matrix layouts/neon-composer.layout.toml
```

The default output directory is `target/preview`. A successful run validates
all four targets and prints native PNG paths like:

```text
target/preview/neon-composer-square-480x480.png
target/preview/neon-composer-portrait-480x1280.png
target/preview/neon-composer-wide-1280x480.png
target/preview/neon-composer-thermalright-curved-2400x1080-2400x1080.png
```

Use `--output-dir target/layout-preview` to keep a named iteration. If any
matrix target fails validation, no matrix PNGs are created; fix the diagnostic
first.

### 4. Inspect images

Check the printed paths and dimensions, then open **all** PNGs with an image
viewer or an image-capable agent. For the four matrix targets, inspect:

- the hero metric's hierarchy and readable unit/value treatment;
- the sparkline's contrast and clipping;
- empty or unavailable values as intentional `--` states;
- whether the composition fills the native canvas without overlap;
- curved local modules staying in left/right readable zones and media bridge
  content being intentional.

The GUI preview adds the readable-zone/protected-bridge overlay. The CLI PNG
is the native rendered frame; use the GUI overlay or Copy Design Context when a
paste-only reviewer also needs the topology explanation.

### 5. Iterate

Edit only declared fields, rerun the one-target JSON validation for fast
feedback, then rerun the full matrix and inspect again. Keep diagnostic JSON,
PNG paths, and the final TOML together when asking an agent for a design review.

## Adding a typed module

A new module is a bounded catalog addition, not a new renderer language. Follow
this path and update every shared boundary:

1. **Document:** add a `FooDocument` with only persisted fields in
   `src/layout_engine/document.rs`; add `Foo(FooDocument)` to
   `ModuleDocument`. Keep `#[serde(deny_unknown_fields)]` and the tagged
   `kind = "foo"` representation. Add parse/serialize and unknown-field tests.
2. **Capabilities:** add `src/layout_engine/modules/foo.rs`, register the
   module in `src/layout_engine/modules/mod.rs`, and expose only finite
   `ModuleCapabilities` (`can_span_bridge`, `supports_binding`,
   `supports_threshold`, `supports_variants`). Do not expose arbitrary style
   maps.
3. **Emitter:** implement `ModuleEmitter` for the runtime module. Resolve only
   typed bindings, emit bounded scene nodes, enforce LCD floors/size/opacity,
   and return a stable `LayoutDiagnostic` on invalid data. The emitter must not
   know whether SVG/resvg or another backend will consume the scene.
4. **Placement and dispatch:** update module-ID matching and any placement
   policy in `src/layout_engine/solver.rs` and `validation.rs`; update
   `src/layout_engine/renderer.rs` to dispatch the document variant to the
   emitter. A bridge-capable module must still require an explicit curved
   profile policy.
5. **GUI metadata:** add the kind and its document fields to
   `gui/src/lib/layout/types.ts`, add its binding/variant/fit/opacity/bridge
   choices to `moduleCapabilities`, and update the module list/inspector so
   the GUI can create, edit, reorder, preview, and diagnose it. The GUI must
   not invent fields the Rust document rejects.
6. **Tests:** cover the document round trip and unknown fields, module
   capabilities and emission, rectangular and curved validation/solve,
   renderer dimensions and missing-data behavior, preview/daemon pixel
   equivalence, and GUI capability/inspector behavior. Extend
   `tests/layout_engine_matrix_tests.rs` when the flagship matrix should cover
   the new kind.

Run focused checks for the changed surface, including the Rust layout-engine
unit tests and `cargo test --test layout_engine_matrix_tests`. Keep new modules
inside this typed path; do not add CSS/SVG embedding, a plugin ABI, freeform
coordinates, or 3D calibration.

## Visual craft for the washed-out LCD

The typed engine deliberately limits mechanics; visual quality still belongs
to the author:

- Favor one clear hero value, a small supporting set, and generous spacing over
  a dense monitoring wall.
- Use a tinted near-black background and a subtly lighter panel. Avoid pure
  `#000000`, which loses depth on hardware.
- Keep foreground colors readable: typed theme foreground channels must meet
  the engine's `#999999` per-channel LCD floor. Use bright accent values,
  quieter units, and clear labels rather than equal brightness everywhere.
- The typed scene enforces opacity of at least `0.7` and text size of at least
  `14px`; design above those floors whenever the physical panel allows it.
- Limit a composition to a few meaningful metrics. If a value is hard to read,
  remove a secondary module before shrinking the hero treatment.
- Check both square/portrait flow and wide two-column balance. Curved previews
  need an intentional bridge image, not ordinary text crossing the protected
  center.

## Non-goals and legacy boundary

This reference does not define arbitrary renderer code, CSS/SVG embedding,
plugin ABIs, undocumented fields, full legacy migration, or 3D calibration.
Legacy SVG/HTML files can remain in the layout directory and continue through
their existing path, but new typed work belongs in `.layout.toml` and the
bounded module catalog above.
