# Contributing

Thanks for taking a look at thermalwriter. The project is still early and has only been hardware-verified on the Thermalright Peerless Vision / GrandVision 360 AIO (`87ad:70db`), so please call out hardware model, distro, and kernel details when reporting behavior.

## Development Setup

- Install Rust 1.85 or newer.
- For GUI work, install Node.js and run `npm ci` in `gui/`.
- Hardware is not required for most changes. Prefer tests and preview examples before commands that talk to USB devices or modify user services.

## Checks

Run the relevant checks before opening a pull request:

```sh
cargo fmt --check
cargo test --workspace
cargo test --workspace --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
cd gui && npm ci && npm run build
```

Rendering changes should also include a generated preview from:

```sh
cargo run --example preview_layout layouts/svg/neon-dash-v2.svg
```

Hardware-facing changes should describe whether they were tested on real hardware. If they were not, say so directly.

## Pull Requests

Use a focused branch and a conventional commit-style title, such as `fix(sensor): handle missing hwmon labels`. Include:

- Summary of behavior changed.
- Tests and preview commands run.
- Hardware impact and hardware tested, if any.
- Screenshots or rendered preview PNGs for layout and GUI changes.

