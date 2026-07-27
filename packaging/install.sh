#!/usr/bin/env bash
# thermalwriter one-shot installer.
#
# Installs either a bundled release binary or a source checkout build to ${CARGO_HOME:-~/.cargo}/bin,
# installs the systemd user service, installs udev rules for USB display and restricted
# RAPL access (prompts for sudo once), and enables + starts the daemon. Idempotent — safe to re-run.
#
# Usage: ./packaging/install.sh   (run as your normal user; do NOT sudo the whole script)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
SYSTEMD_USER_DIR="$HOME/.config/systemd/user"
INSTALLED_BIN="$CARGO_BIN/thermalwriter"
INSTALLED_TRAY_BIN="$CARGO_BIN/thermalwriter-tray"
PREBUILT_BIN="$PROJECT_DIR/bin/thermalwriter"
PREBUILT_TRAY_BIN="$PROJECT_DIR/bin/thermalwriter-tray"
# Install the lightweight StatusNotifier tray by default (set INSTALL_TRAY=0 to skip).
INSTALL_TRAY="${INSTALL_TRAY:-1}"

if [[ $EUID -eq 0 ]]; then
    echo "Run this as your normal user, not root. It will prompt for sudo when needed." >&2
    exit 1
fi

if ! command -v systemctl >/dev/null 2>&1; then
    echo "error: systemctl not found — thermalwriter requires systemd user services" >&2
    exit 1
fi

if ! command -v install >/dev/null 2>&1; then
    echo "error: install command not found" >&2
    exit 1
fi

if ! command -v sudo >/dev/null 2>&1; then
    echo "error: sudo not found — setup-udev needs sudo for /etc/udev/rules.d" >&2
    exit 1
fi

if [[ ! -x "$PREBUILT_BIN" ]]; then
    if [[ ! -f "$PROJECT_DIR/Cargo.toml" ]]; then
        echo "error: neither bundled binary ($PREBUILT_BIN) nor Cargo.toml source checkout found" >&2
        exit 1
    fi

    if ! command -v cargo >/dev/null 2>&1; then
        echo "error: cargo not found on PATH — install Rust first (https://rustup.rs)" >&2
        exit 1
    fi

    if ! command -v pkg-config >/dev/null 2>&1; then
        echo "error: pkg-config not found — install pkg-config or pkgconf before building thermalwriter" >&2
        exit 1
    fi

    if ! pkg-config --exists libudev; then
        echo "error: libudev development files not found — install libudev-dev on Debian/Ubuntu, systemd-devel on Fedora, or systemd on Arch" >&2
        exit 1
    fi
fi

if ! systemctl --user show-environment >/dev/null 2>&1; then
    echo "error: systemd user session is not available; run from a logged-in desktop/user session" >&2
    exit 1
fi

if [[ -x "$PREBUILT_BIN" ]]; then
    echo "==> Installing bundled thermalwriter binary..."
    mkdir -p "$CARGO_BIN"
    install -m 0755 "$PREBUILT_BIN" "$INSTALLED_BIN"
else
    echo "==> Building and installing thermalwriter binary..."
    ( cd "$PROJECT_DIR" && cargo install --path . --locked )
fi
if [[ ! -x "$INSTALLED_BIN" ]]; then
    echo "error: expected installed binary at $INSTALLED_BIN" >&2
    exit 1
fi

if [[ "$INSTALL_TRAY" == "1" ]]; then
    if [[ -x "$PREBUILT_TRAY_BIN" ]]; then
        echo "==> Installing bundled thermalwriter-tray binary..."
        mkdir -p "$CARGO_BIN"
        install -m 0755 "$PREBUILT_TRAY_BIN" "$INSTALLED_TRAY_BIN"
    elif [[ -f "$PROJECT_DIR/tray/Cargo.toml" ]]; then
        echo "==> Building and installing thermalwriter-tray binary..."
        ( cd "$PROJECT_DIR" && cargo install --path tray --locked )
    else
        echo "==> Skipping tray install (no tray sources or bundled binary)"
        INSTALL_TRAY=0
    fi
fi

echo "==> Installing systemd user service..."
mkdir -p "$SYSTEMD_USER_DIR"
SYSTEMD_EXEC_BIN="${INSTALLED_BIN//\\/\\\\}"
SYSTEMD_EXEC_BIN="${SYSTEMD_EXEC_BIN//\"/\\\"}"
SYSTEMD_EXEC_BIN="${SYSTEMD_EXEC_BIN//%/%%}"
cat > "$SYSTEMD_USER_DIR/thermalwriter.service" <<EOF
[Unit]
Description=Thermalright Cooler LCD Display Service
Documentation=https://github.com/mgaruccio/thermalwriter
After=default.target

[Service]
Type=simple
ExecStart="$SYSTEMD_EXEC_BIN" daemon
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
EOF
chmod 0644 "$SYSTEMD_USER_DIR/thermalwriter.service"
echo "    ExecStart=$INSTALLED_BIN daemon"
systemctl --user daemon-reload

echo "==> Installing udev rules for USB display and RAPL access (sudo required)..."
"$INSTALLED_BIN" setup-udev

echo "==> Enabling and (re)starting the service..."
systemctl --user enable thermalwriter.service
systemctl --user restart thermalwriter.service

if [[ "$INSTALL_TRAY" == "1" && -x "$INSTALLED_TRAY_BIN" ]]; then
    echo "==> Installing thermalwriter-tray user service..."
    SYSTEMD_TRAY_BIN="${INSTALLED_TRAY_BIN//\\/\\\\}"
    SYSTEMD_TRAY_BIN="${SYSTEMD_TRAY_BIN//\"/\\\"}"
    SYSTEMD_TRAY_BIN="${SYSTEMD_TRAY_BIN//%/%%}"
    cat > "$SYSTEMD_USER_DIR/thermalwriter-tray.service" <<EOF
[Unit]
Description=Thermalwriter system tray controller
Documentation=https://github.com/mgaruccio/thermalwriter
After=graphical-session.target thermalwriter.service
Wants=thermalwriter.service
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart="$SYSTEMD_TRAY_BIN"
Restart=on-failure
RestartSec=3
Environment=RUST_LOG=info

[Install]
WantedBy=graphical-session.target
EOF
    chmod 0644 "$SYSTEMD_USER_DIR/thermalwriter-tray.service"
    systemctl --user daemon-reload

    # XDG autostart fallback for desktops that do not start graphical-session target units.
    AUTOSTART_DIR="$HOME/.config/autostart"
    mkdir -p "$AUTOSTART_DIR"
    if [[ -f "$PROJECT_DIR/packaging/thermalwriter-tray.desktop" ]]; then
        sed "s|^Exec=.*|Exec=$INSTALLED_TRAY_BIN|" \
            "$PROJECT_DIR/packaging/thermalwriter-tray.desktop" \
            > "$AUTOSTART_DIR/thermalwriter-tray.desktop"
        chmod 0644 "$AUTOSTART_DIR/thermalwriter-tray.desktop"
    fi

    if systemctl --user enable --now thermalwriter-tray.service 2>/dev/null; then
        echo "    tray service enabled (graphical-session.target)"
    else
        echo "    note: could not enable tray systemd unit; XDG autostart desktop entry installed"
        echo "    start manually: $INSTALLED_TRAY_BIN &"
    fi
fi

echo
echo "Done. Status:"
systemctl --user --no-pager --lines=0 status thermalwriter.service || true
if [[ "$INSTALL_TRAY" == "1" ]]; then
    systemctl --user --no-pager --lines=0 status thermalwriter-tray.service 2>/dev/null || true
fi
echo
echo "Useful follow-ups:"
echo "  thermalwriter ctl status"
echo "  journalctl --user -u thermalwriter -f"
if [[ "$INSTALL_TRAY" == "1" ]]; then
    echo "  thermalwriter-tray          # system tray (Open Config, layouts, stream presets)"
    echo "  INSTALL_TRAY=0 $0           # reinstall without tray"
fi
