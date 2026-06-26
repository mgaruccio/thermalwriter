# Release Checklist

## GitHub Readiness

- Confirm `README.md`, `LICENSE`, `CONTRIBUTING.md`, and `SECURITY.md` are current.
- Confirm supported hardware and untested hardware are clearly stated.
- Run the full verification set from `CONTRIBUTING.md`.
- Generate at least one layout preview PNG for visual changes.

## Crate Packaging

Run:

```sh
cargo package --list
cargo package
```

Inspect the file list. It should include source, examples, tests, built-in layouts/assets, packaging files, and public docs. It should not include `target/`, worktrees, agent-local folders, `gui/node_modules/`, `gui/dist/`, or old implementation plans under `docs/plans/`.

Publish the daemon crate only after a clean package build:

```sh
cargo publish
```

The Tauri GUI crate is marked `publish = false`; distribute the GUI through Tauri bundles instead of crates.io.

## GUI App Distribution

Before producing GUI artifacts locally:

```sh
cd gui
npm ci
npm run tauri:build
```

Expected bundle output directories:
- `.AppImage` bundle: `gui/src-tauri/target/release/bundle/appimage/`
- `.deb` bundle: `gui/src-tauri/target/release/bundle/deb/`

## Tag-Release Artifacts

GitHub release automation runs on tag push (matching `v*`) or `workflow_dispatch`. The following artifacts are compiled, packaged, hashed, and uploaded to the GitHub Release:

1. **Daemon Tarball**: `thermalwriter-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` containing:
   - `bin/thermalwriter` (release binary)
   - `README.md`
   - `LICENSE`
   - `packaging/install.sh`
   - `packaging/uninstall.sh`
   - `packaging/udev/99-thermalwriter-rapl.rules`
   - `systemd/thermalwriter.service`
2. **GUI Debian Package**: `*.deb`
3. **GUI AppImage**: `*.AppImage`
4. **Checksums**: `SHA256SUMS` containing hashes of all uploaded release files.
