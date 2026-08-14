#!/usr/bin/env bash
# Remove the user service and installed thermalwriter binary.
#
# This intentionally leaves ~/.config/thermalwriter/ in place so user layouts,
# backgrounds, and config are not deleted by surprise.

set -euo pipefail

CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
SYSTEMD_USER_DIR="$HOME/.config/systemd/user"
UDEV_RULE="/etc/udev/rules.d/99-thermalwriter-rapl.rules"

if [[ $EUID -eq 0 ]]; then
    echo "Run this as your normal user, not root. It will prompt for sudo for the udev rule." >&2
    exit 1
fi

echo "==> Stopping and disabling thermalwriter user services..."
systemctl --user disable --now thermalwriter-tray.service 2>/dev/null || true
systemctl --user disable --now thermalwriter.service 2>/dev/null || true

echo "==> Removing systemd user services..."
rm -f "$SYSTEMD_USER_DIR/thermalwriter.service"
rm -f "$SYSTEMD_USER_DIR/thermalwriter-tray.service"
systemctl --user daemon-reload

echo "==> Removing autostart and launcher entries..."
rm -f "$HOME/.config/autostart/thermalwriter-tray.desktop"
rm -f "$HOME/.local/share/applications/thermalwriter-config.desktop"
rm -f "$HOME/.local/share/icons/hicolor/"{32x32,64x64,128x128}"/apps/thermalwriter-config.png"

echo "==> Removing installed binaries..."
rm -f "$CARGO_BIN/thermalwriter"
rm -f "$CARGO_BIN/thermalwriter-gui"
rm -f "$CARGO_BIN/thermalwriter-tray"

if [[ -f "$UDEV_RULE" ]]; then
    echo "==> Removing thermalwriter udev rule (sudo required)..."
    sudo rm -f "$UDEV_RULE"
    sudo udevadm control --reload-rules
    echo "Left group 'thermalreader' in place; remove it manually with 'sudo groupdel thermalreader' if no longer needed."
fi

echo
echo "Uninstalled thermalwriter."
echo "User config was left in ~/.config/thermalwriter/"
