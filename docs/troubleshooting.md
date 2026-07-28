# Troubleshooting

Practical fixes for common thermalwriter install, daemon, GUI, and streaming issues on Linux.

## Quick diagnostics

```sh
thermalwriter --version
thermalwriter ctl status
systemctl --user status thermalwriter
journalctl --user -u thermalwriter -n 100 --no-pager
```

Collect a bundle for bug reports:

```sh
mkdir -p /tmp/tw-debug
{
  echo "=== thermalwriter --version ==="
  thermalwriter --version 2>&1 || true
  echo "=== thermalwriter ctl status ==="
  thermalwriter ctl status 2>&1 || true
  echo "=== systemctl --user status thermalwriter ==="
  systemctl --user --no-pager status thermalwriter 2>&1 || true
  echo "=== journalctl (last 200) ==="
  journalctl --user -u thermalwriter -n 200 --no-pager 2>&1 || true
  echo "=== config.toml ==="
  cat ~/.config/thermalwriter/config.toml 2>&1 || true
  echo "=== lsusb (display) ==="
  lsusb 2>&1 || true
  echo "=== groups ==="
  id 2>&1 || true
} > /tmp/tw-debug/report.txt
tar -C /tmp -czf tw-debug.tar.gz tw-debug
```

## Daemon and session bus

**`Could not connect to D-Bus session bus`** — run `thermalwriter ctl` from the same graphical login session as the daemon. SSH without a user bus needs `XDG_RUNTIME_DIR` and an imported session (see `scripts/release-qa/guest/common-assert.sh` for the linger pattern).

**Service inactive after install** — check `journalctl --user -u thermalwriter`. Common causes: binary path mismatch (re-run `./packaging/install.sh`), stale unit from an old `CARGO_HOME`, or missing `systemd --user` session.

**`thermalwriter ctl status` hangs or returns ServiceUnknown** — the daemon may still be starting; wait a few seconds. If it persists, `systemctl --user restart thermalwriter`.

## Device and udev

**`connected: false` with hardware attached** — replug the cooler after `thermalwriter setup-udev`. Confirm the rule exists:

```sh
ls -l /etc/udev/rules.d/99-thermalwriter-rapl.rules
lsusb | grep -iE '87ad|87cd|0402|0416|0418'
```

**Permission denied opening USB** — udev `uaccess` tags require a active session. Log out/in after install, or replug the device.

**Wrong cooler when multiple displays share a VID:PID** — set `display.device = "VID:PID"` in `config.toml` (hex, lowercase). Distinct IDs can be selected explicitly; identical IDs cannot be disambiguated yet.

## RAPL / CPU power group

**CPU power shows unavailable** — install the udev rule and join the `thermalreader` group:

```sh
thermalwriter setup-udev
# log out and back in so the user session inherits the group
groups | grep thermalreader
```

Without the rule the daemon still runs; package power metrics are simply omitted.

## System tray without Config GUI

**No tray icon after install** — default `INSTALL_TRAY=auto` skips the tray when no Config GUI is found. Install the GUI (`.deb` / `.AppImage`) and re-run `./packaging/install.sh`, or force the tray menu:

```sh
INSTALL_TRAY=1 ./packaging/install.sh
```

**Tray double-starts** — the installer enables **either** a systemd user unit **or** an XDG autostart entry, not both. The binary also single-instances via `$XDG_RUNTIME_DIR/thermalwriter/tray.lock`.

**Tray visible but Open Config fails** — point at a non-PATH GUI:

```sh
export THERMALWRITER_GUI=/path/to/Thermalwriter-Config_*.AppImage
thermalwriter-tray &
```

See `docs/gui.md` for Hyprland/Noctalia, GNOME AppIndicator, and KDE Plasma notes.

## AppImage, FUSE, and graphics

Prebuilt GUI artifacts target **x86_64 Linux with glibc ≥ 2.35** (built on Ubuntu 22.04). Check your system:

```sh
ldd --version | head -1
```

**AppImage does not start / FUSE errors** — AppImages normally require FUSE (`fuse2` or `libfuse2`). Fallback without mounting:

```sh
chmod +x ./Thermalwriter-Config_*_amd64.AppImage
APPIMAGE_EXTRACT_AND_RUN=1 ./Thermalwriter-Config_*_amd64.AppImage
# or:
./Thermalwriter-Config_*_amd64.AppImage --appimage-extract-and-run
```

**Blank or corrupted GUI window on Wayland** — the GUI sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` at startup as a known WebKit workaround. If issues persist, try launching under an X11 session or from the `.deb` package.

**Not fully standalone** — release AppImages bundle WebKitGTK/GTK from the linuxdeploy pipeline but still expect host graphics stack compatibility (Mesa, fontconfig, FUSE). The `.deb` installs declared dependencies; the daemon tarball is the most portable daemon artifact.

## Streaming and Xvfb

**Stream preset fails immediately** — install `xvfb` (`xorg-server-xvfb`). Confirm the daemon resolves the binary:

```sh
thermalwriter ctl status
# built-in presets need conky/cava/btop on the daemon's PATH
```

**SDL apps crash under stream** — the daemon injects `SDL_VIDEODRIVER=x11` for streamed children when the session has `WAYLAND_DISPLAY` set.

**Stream not restored after reboot** — streaming is session-only by design; the daemon boots from the saved layout in `config.toml`.

## glibc compatibility

If a prebuilt binary fails with `version 'GLIBC_2.36' not found` (or similar), your distro glibc is older than the build baseline. Options:

- Upgrade the host (or use a newer container/chroot)
- Build from source: `./packaging/install.sh` in a source checkout
- Check the highest required symbol: `objdump -T ./bin/thermalwriter | grep GLIBC_ | sort -V | tail -5`

Release CI pins **ubuntu-22.04** (glibc 2.35) and L0 QA asserts daemon, tray, and GUI binaries require GLIBC ≤ 2.35.

## Logs and config reset

**Config path** — `~/.config/thermalwriter/config.toml` (layouts, backgrounds, wrappers alongside).

**Uninstall without deleting config**:

```sh
./packaging/uninstall.sh
```

**Full reset** (removes user config — back up first):

```sh
./packaging/uninstall.sh
rm -rf ~/.config/thermalwriter
systemctl --user daemon-reload
```

**Hardware-free daemon testing**:

```sh
THERMALWRITER_TRANSPORT=null THERMALWRITER_PROFILE=grand-vision-480 cargo run -- daemon
```

## Getting help

Open an issue with `thermalwriter --version`, distro/kernel, cooler `lsusb` line, `thermalwriter ctl status`, and relevant `journalctl` excerpts. For device coverage gaps, use the device report template linked from the README.
