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

GitHub release automation runs on:

| Trigger | Prerelease? | Notes |
| --- | --- | --- |
| **Tag push** (`v*`) | **No** — becomes GitHub **Latest** | Normal releases |
| **workflow_dispatch** | Configurable (`prerelease` input) | Explicit tag + optional prerelease flag |

**Builder image**: `ubuntu-22.04` (glibc 2.35 baseline for prebuilt x86_64 binaries).

Release notes are extracted from the matching `CHANGELOG.md` section (`## [X.Y.Z]`), not the entire file.

The following artifacts are compiled, packaged, hashed, and uploaded:

1. **Daemon Tarball**: `thermalwriter-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` containing:
   - `bin/thermalwriter` (release binary)
   - `bin/thermalwriter-tray` (StatusNotifier tray controller)
   - `README.md`
   - `LICENSE`
   - `THIRD_PARTY_NOTICES.md`
   - `packaging/install.sh`
   - `packaging/lib/tray-install.sh`
   - `packaging/uninstall.sh`
   - `packaging/thermalwriter-tray.desktop`
   - `packaging/udev/99-thermalwriter-rapl.rules`
   - `systemd/thermalwriter.service`
   - `systemd/thermalwriter-tray.service`
   - public docs under `docs/` (including `comparison-methodology.md`, `troubleshooting.md`)
4. **GUI Debian Package**: `*.deb`
5. **GUI AppImage**: `*.AppImage` (bundles WebKitGTK/GTK; not fully standalone — document FUSE / `APPIMAGE_EXTRACT_AND_RUN=1`)
6. **Checksums**: `SHA256SUMS` containing hashes of all uploaded release files.

**L0 QA** (`scripts/release-qa/host/run-l0.sh`) additionally verifies GLIBC ≤ 2.35 on daemon/tray/GUI binaries and asserts release GUI artifacts contain no MCP bridge strings.

## Clean-Machine Release QA (#90)

Before public announcement posts, validate the **tagged release artifacts** (not a dirty worktree) with the harness under `scripts/release-qa/`.

```sh
# L0 — checksums, tarball layout, README relative links (no VMs)
./scripts/release-qa/host/run-l0.sh v0.1.3

# L1 — Ubuntu 24.04 tarball + GUI packages; Arch source install + AppImage
# Requires KVM, qemu-system-x86_64, qemu-img, cloud-localds (cloud-image-utils).
./scripts/release-qa/host/run-all.sh v0.1.3

# Tray SNI smoke on Ubuntu GNOME AppIndicator + Arch KDE Plasma watcher hosts
./scripts/release-qa/host/run-tray-desktop.sh v0.1.3

# L2 — host cooler unplug/replug (connected transitions); interactive
./scripts/release-qa/host/hw-attach-smoke.sh
```

Reports land in `scripts/release-qa/out/<tag>/summary.md`. Full guest mapping and env overrides: `scripts/release-qa/README.md`.

**#90 acceptance** = L0 + L1 multi-distro install matrix (Ubuntu LTS tarball/GUI, Arch source/AppImage). L2 host unplug/replug is optional host tooling and is not required to close the issue.

