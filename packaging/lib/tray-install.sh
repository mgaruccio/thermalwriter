#!/usr/bin/env bash
# Shared tray install helpers for packaging/install.sh and shell tests.
# shellcheck shell=bash

# Return 0 when a Config GUI binary is discoverable on this machine.
tw_discover_config_gui() {
    local candidate dir appdir

    if command -v thermalwriter-gui >/dev/null 2>&1; then
        command -v thermalwriter-gui
        return 0
    fi

    local search_dirs=(
        "${HOME}/.local/bin"
        "${CARGO_HOME:-${HOME}/.cargo}/bin"
        "/usr/local/bin"
        "/usr/bin"
    )
    for dir in "${search_dirs[@]}"; do
        if [[ -x "$dir/thermalwriter-gui" ]]; then
            printf '%s\n' "$dir/thermalwriter-gui"
            return 0
        fi
    done

    for appdir in "${HOME}/Applications" "${HOME}/Downloads"; do
        [[ -d "$appdir" ]] || continue
        shopt -s nullglob
        for candidate in \
            "$appdir"/Thermalwriter*.AppImage \
            "$appdir"/thermalwriter*.AppImage; do
            if [[ -x "$candidate" ]]; then
                printf '%s\n' "$candidate"
                return 0
            fi
        done
        shopt -u nullglob
    done

    return 1
}

# Normalize INSTALL_TRAY (auto|0|1) into 0 or 1. Prints user-facing status lines.
tw_resolve_install_tray() {
    local mode="${1:-auto}"
    local gui_path=""

    case "$mode" in
        0|false|no|off)
            printf '0'
            return 0
            ;;
        1|true|yes|on)
            printf '1'
            return 0
            ;;
        auto|"")
            if gui_path="$(tw_discover_config_gui)"; then
                printf '1'
                return 0
            fi
            printf '0'
            return 0
            ;;
        *)
            printf 'error: INSTALL_TRAY must be auto, 0, or 1 (got: %s)\n' "$mode" >&2
            return 2
            ;;
    esac
}

# Disable and remove a previously installed tray (idempotent).
tw_remove_tray_install() {
    local cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
    local systemd_dir="$HOME/.config/systemd/user"
    local autostart="$HOME/.config/autostart/thermalwriter-tray.desktop"
    local removed=0

    if command -v systemctl >/dev/null 2>&1; then
        if systemctl --user disable --now thermalwriter-tray.service 2>/dev/null; then
            removed=1
        fi
    fi
    if [[ -f "$systemd_dir/thermalwriter-tray.service" ]]; then
        rm -f "$systemd_dir/thermalwriter-tray.service"
        removed=1
    fi
    if [[ -f "$autostart" ]]; then
        rm -f "$autostart"
        removed=1
    fi
    if [[ -x "$cargo_bin/thermalwriter-tray" ]]; then
        rm -f "$cargo_bin/thermalwriter-tray"
        removed=1
    fi
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user daemon-reload 2>/dev/null || true
    fi
    if [[ "$removed" -eq 1 ]]; then
        echo "==> Removed previously installed tray service/autostart/binary"
    fi
}

tw_install_tray_mode_message() {
    local mode="${1:-auto}"
    local resolved="${2:-0}"
    local gui_path=""

    case "$mode" in
        0|false|no|off)
            echo "==> Tray install skipped (INSTALL_TRAY=0)"
            ;;
        1|true|yes|on)
            echo "==> Tray install forced (INSTALL_TRAY=1)"
            ;;
        auto|"")
            if [[ "$resolved" == "1" ]] && gui_path="$(tw_discover_config_gui)"; then
                echo "==> Config GUI found ($gui_path); installing tray"
            else
                echo "==> Tray install skipped (INSTALL_TRAY=auto, no Config GUI found)"
                echo "    Install the GUI (.deb/.AppImage), then re-run install.sh,"
                echo "    or set INSTALL_TRAY=1 for the GUI-less tray menu."
            fi
            ;;
    esac
}
