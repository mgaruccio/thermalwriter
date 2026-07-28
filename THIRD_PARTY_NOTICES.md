# Third-Party Notices

thermalwriter bundles or depends on the following third-party components. Full license texts for vendored fonts are included alongside the font files under `gui/src/fonts/`.

## Daemon layout renderer — DejaVu Sans Mono

- **Component**: DejaVu Sans Mono (embedded in daemon SVG/HTML rendering)
- **File**: `assets/fonts/JetBrainsMono-Regular.ttf` (historical filename; contents are DejaVu Sans Mono)
- **License**: Bitstream Vera / DejaVu Fonts License (`assets/fonts/DejaVu-LICENSE.txt`)
- **Usage**: Embedded via `include_bytes!` in `src/render/svg.rs` and `src/render/draw.rs` for LCD layout text rendering

## Config GUI — IBM Plex Mono

- **Component**: IBM Plex Mono (Light, Regular, Medium, SemiBold, Bold)
- **Files**: `gui/src/fonts/IBMPlexMono-*.ttf`
- **License**: SIL Open Font License 1.1 (`gui/src/fonts/OFL-IBMPlexMono.txt`)
- **Source**: [google/fonts ofl/ibmplexmono](https://github.com/google/fonts/tree/main/ofl/ibmplexmono)

## Config GUI — IBM Plex Sans

- **Component**: IBM Plex Sans (Regular, Medium, SemiBold, Bold)
- **Files**: `gui/src/fonts/IBMPlexSans-*.ttf`
- **License**: SIL Open Font License 1.1 (`gui/src/fonts/OFL-IBMPlexSans.txt`)
- **Source**: [IBM/plex](https://github.com/IBM/plex) release `@ibm/plex-sans@1.1.0`

## Config GUI — Major Mono Display

- **Component**: Major Mono Display Regular
- **File**: `gui/src/fonts/MajorMonoDisplay-Regular.ttf`
- **License**: SIL Open Font License 1.1 (`gui/src/fonts/OFL-MajorMonoDisplay.txt`)
- **Source**: [google/fonts ofl/majormonodisplay](https://github.com/google/fonts/tree/main/ofl/majormonodisplay)

## Other dependencies

Runtime libraries (Rust crates, WebKitGTK/GTK in GUI bundles, system `libudev`, etc.) are listed in `Cargo.lock`, `gui/package-lock.json`, and the respective package manifests. See each project's license for redistribution terms.
