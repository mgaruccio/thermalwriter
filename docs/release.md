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

## Clean-Machine Release QA (#90)

Before public announcement posts, validate the **tagged release artifacts** (not a dirty worktree) with the harness under `scripts/release-qa/`.

```sh
# L0 — checksums, tarball layout, README relative links (no VMs)
./scripts/release-qa/host/run-l0.sh v0.1.1

# L1 — Ubuntu 24.04 tarball + GUI packages; Arch source install + AppImage
# Requires KVM, qemu-system-x86_64, qemu-img, cloud-localds (cloud-image-utils).
./scripts/release-qa/host/run-all.sh v0.1.1

# L2 — host cooler unplug/replug (connected transitions); interactive
./scripts/release-qa/host/hw-attach-smoke.sh
```

Reports land in `scripts/release-qa/out/<tag>/summary.md`. Full guest mapping and env overrides: `scripts/release-qa/README.md`.

L1 scopes hardware-free install paths. Detach/reattach and "udev after replug" are L2 on bare metal (USB passthrough into QEMU is intentionally not required to close the install gate).

