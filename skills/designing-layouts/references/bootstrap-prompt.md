# Thermalwriter layout-agent bootstrap prompt

Copy the prompt below into a repository-capable coding agent such as OpenAI Codex or
Claude Code. The complete schema is [layout-toml.md](./layout-toml.md). It is also
usable in a free chatbot: that branch does not assume a repository, shell, image
viewer, hidden system prompt, or LLM service. The model must be honest about which
steps it could and could not perform.

## Copyable prompt

```text
You are designing a Thermalwriter `.layout.toml` for the typed layout engine.
Produce a useful, attractive LCD composition, then check your own work through the
real authoring loop. Do not design SVG, HTML, CSS, Tera templates, pixel
coordinates, arbitrary styles, or a new renderer language.

SOURCE OF TRUTH
- Use `skills/designing-layouts/references/layout-toml.md` for the complete,
  current schema and commands. Start from `layouts/neon-composer.layout.toml`.
- The document has `version = 1`, a safe `name`, ordered `[[modules]]`, and
  profile tables. Modules are bounded typed `metric`, `sparkline`, `text`, or
  `media` entries with stable `id`, `kind`, `binding`, and `variant` fields.
- Use only documented bindings, variants, media fields, and profiles. Text is a
  bound runtime value, not a free-form template. Media sources stay below the
  approved layout/media directory and must not use `..`, symlink escapes, or an
  unbounded external path.
- The stable targets are `square` (480x480, `column`), `portrait` (480x1280,
  `column`), `wide` (1280x480, `two-column`), and
  `thermalright-curved-2400x1080` (2400x1080,
  `zoned-panorama`). Never add coordinates or undeclared keys.

VISUAL RULES FOR THE WASHED-OUT LCD
- Prefer a tinted near-black background (not pure `#000000`) and a subtly
  lighter panel. Keep typed foreground channels at or above the engine's
  `#999999` per-channel readability floor.
- Use one clear hero value, a small supporting set, short labels, generous
  spacing, bright accents for important values, quieter units, and neutral
  labels. Remove a secondary module before shrinking text until it is unreadable.
- Respect the typed scene floors: opacity is at least 0.7 and text is at least
  14px. Design above those floors when the physical panel allows it.
- Curved topology is conservative, not optical calibration: left-readable is
  x=0..960, center-bridge is x=960..1440 and protected, and right-readable is
  x=1440..2400. Keep metric, sparkline, and text modules local to readable
  zones. Use bridge spanning only for an intentional `media` module with
  `span_bridge = true` and an explicit `media-only` (or documented capable)
  policy; prefer `local-only` when bridge content is not needed.

REQUIRED LOOP — DO NOT SKIP A STEP
1. GENERATE. Copy the shipped preset to a new `.layout.toml`, or produce a
   bounded document using only the schema above. Keep module IDs stable while
   iterating. If you have no repository or file-writing tools, ask the user for
   the schema/reference or a `Copy Design Context` output before inventing any
   field, then return a complete candidate document for the user to save.
2. VALIDATE DETERMINISTICALLY. Use the real preview example; it validates the
   requested target before rendering and there is no separate validator to
   invent. For fast machine-readable diagnostics, run:
   `cargo run --example preview_layout -- --format json --profile square <path>`
   Fix every error (and meaningful warning) using its code, profile, module,
   property, reason, and fix fields. Use the same command with each profile
   while debugging a target.
3. PREVIEW ALL TARGETS. Run the required matrix, not just the target that looks
   easiest:
   `cargo run --example preview_layout -- --matrix <path>`
   Optionally use `--output-dir target/layout-preview-<iteration>` to retain
   separate iterations. Confirm that the command validates and renders square,
   portrait, wide, and the explicit curved profile, and record every PNG path it
   prints.
4. INSPECT THE ACTUAL OUTPUT. Open every generated PNG at native dimensions;
   do not treat a successful exit status, diagnostic JSON, or file existence as
   visual proof. Check hierarchy, value/unit readability, LCD contrast,
   clipping, overflow, empty `--` states, spacing, two-column balance, and
   curved left/right placement. Confirm that any bridge image is intentional
   and that ordinary modules never cross the protected bridge. If you have
   repository and image-viewer access, perform this inspection yourself.
5. REVISE. Fix the TOML (only with declared fields), rerun deterministic
   validation, render the complete matrix again, open every new PNG, and repeat
   until all targets are both valid and visually legible. Report the final TOML,
   diagnostics, and exact preview paths. Never claim that an image was opened
   or a command was run when you could not actually do it.

NO-TOOLS / PASTE-ONLY BRANCH
If you cannot access the repository, shell, GUI, or an image viewer, do not
pretend that you generated, validated, rendered, or inspected anything. Ask the
user to paste the exact artifacts available from the GUI or capable environment:
`Copy Error`, `Copy Preview Image` (for each target, or the actual PNG images),
and `Copy Design Context`, plus the current `.layout.toml` and any command output.
A free chatbot can then reason from those pasted errors, context, and images:
propose a bounded TOML revision, explain the expected commands, and ask the user
to run the commands and paste the next artifacts. Continue the same
generate -> validate -> preview all targets -> inspect -> revise loop across
those paste/reply rounds. If a preview image is not pasted, mark visual
inspection as pending rather than accepting the design.

REVIEW BOUNDARY
Deterministic TOML validation and the native four-target preview matrix are the
landed authoring checks. Human or vision review may be used as an optional
release-quality supplement after the matrix, but a vision gate is not landed:
do not make it an automatic acceptance rule and do not auto-accept model output.
```
