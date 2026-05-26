# Release Checklist

## GitHub Readiness

- Confirm `README.md`, `LICENSE`, `CONTRIBUTING.md`, and `SECURITY.md` are current.
- Confirm supported hardware and untested hardware are clearly stated.
- Run the full verification set from `CONTRIBUTING.md`.
- Generate at least one layout preview PNG for visual changes.

## Crate Packaging

Run:

```sh
cargo package --list --allow-dirty
cargo package --allow-dirty
```

Inspect the file list. It should include source, examples, tests, built-in layouts/assets, packaging files, and public docs. It should not include `target/`, worktrees, agent-local folders, `gui/node_modules/`, `gui/dist/`, or old implementation plans.

Publish the daemon crate only after a clean package build:

```sh
cargo publish
```

The Tauri GUI crate is marked `publish = false`; distribute the GUI through Tauri bundles instead of crates.io.

## GUI App Distribution

Before producing GUI artifacts:

```sh
cd gui
npm ci
npm run build
npm run tauri -- build
```

Document the generated package type, distro tested, and whether the daemon service was already installed.

