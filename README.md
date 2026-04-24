# thermalwriter

A lightweight Rust daemon (~18 MB binary, ~30 MB RSS, ~1% CPU at 2 FPS) that drives
Thermalright cooler LCD displays — a drop-in replacement for the 400 MB Python/Qt `trcc`
app that ships with the cooler.

## Status

v0.1.0 — running as a systemd user service; hardware-verified on the Thermalright
Peerless Vision ("GrandVision 360 AIO", USB `87ad:70db`).

## Requirements

- Linux (systemd + udev)
- Rust 1.82+ (edition 2024)
- A supported Thermalright cooler on USB
- D-Bus session bus (standard on any desktop install)

## Install

One-shot, from a clone:

```sh
./packaging/install.sh
```

This:

1. Builds and installs `thermalwriter` to `~/.cargo/bin`.
2. Installs the systemd user service to `~/.config/systemd/user/thermalwriter.service`.
3. Installs a udev rule to `/etc/udev/rules.d/99-thermalwriter-rapl.rules` (**prompts for
   sudo** — needed so the daemon can read `/sys/class/powercap/intel-rapl:*/energy_uj`
   for CPU power; see note below).
4. Enables and starts the service.

The script is idempotent — re-run it to upgrade.

### Why the udev rule?

Since CVE-2020-8694 ("Platypus") Linux defaults `energy_uj` to mode `0400`. The daemon
runs as your user, so without the rule CPU power shows up as `--` on the display. The
rule chmod's `energy_uj` to `0444` on every `add|change` event, which survives PM / driver
events that a boot-only `tmpfiles.d` entry would not.

If you skip the udev rule, the daemon still runs — `cpu_power` just stays `--`, and
a warning is logged on first poll pointing you to `thermalwriter setup-udev`.

### Manual install

If you'd rather do it yourself:

```sh
cargo install --path . --locked
install -Dm0644 systemd/thermalwriter.service ~/.config/systemd/user/thermalwriter.service
systemctl --user daemon-reload
thermalwriter setup-udev                       # sudo for the udev rule only
systemctl --user enable --now thermalwriter
```

## Usage

```sh
systemctl --user status thermalwriter      # service status
thermalwriter ctl status                   # query daemon over D-Bus
thermalwriter ctl layouts                  # list available layouts
thermalwriter ctl layout svg/neon-dash-v2.svg
thermalwriter ctl mirror "conky -c ~/.config/conky/lcd.conf"  # X11 capture mode
```

Config lives at `~/.config/thermalwriter/config.toml`; layouts in
`~/.config/thermalwriter/layouts/` (built-ins seeded on first run).

## Development

See [CLAUDE.md](./CLAUDE.md) for architecture, layout authoring, and the full command
reference (benchmarks, examples, layout preview/render tooling).

## License

TBD.
