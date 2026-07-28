#!/usr/bin/env bash
# Shared assertions for guest install tests. Sourced by guest scripts.
# shellcheck shell=bash

set -euo pipefail

GUEST_FAIL=0
guest_pass() { printf 'PASS  %s\n' "$*"; }
guest_fail() { printf 'FAIL  %s\n' "$*" >&2; GUEST_FAIL=1; }
guest_info() { printf '    %s\n' "$*"; }
guest_log() { printf '==> %s\n' "$*"; }

guest_require_cmd() {
    local c
    for c in "$@"; do
        command -v "$c" >/dev/null 2>&1 || { guest_fail "missing command: $c"; return 1; }
    done
}

# Ensure a systemd --user bus is available over SSH (linger + dbus import).
guest_ensure_user_systemd() {
    guest_log "ensuring systemd --user session"
    if ! command -v systemctl >/dev/null 2>&1; then
        guest_fail "systemctl not found"
        return 1
    fi

    # Linger so user services survive without a graphical login.
    if command -v loginctl >/dev/null 2>&1; then
        sudo loginctl enable-linger "$USER" 2>/dev/null || true
    fi

    # Import environment that systemd --user needs when invoked over SSH.
    export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    if [[ ! -d "$XDG_RUNTIME_DIR" ]]; then
        sudo mkdir -p "$XDG_RUNTIME_DIR"
        sudo chown "$USER:$USER" "$XDG_RUNTIME_DIR"
        sudo chmod 700 "$XDG_RUNTIME_DIR"
    fi

    # Start user manager if needed.
    if ! systemctl --user show-environment >/dev/null 2>&1; then
        systemctl --user daemon-reload 2>/dev/null || true
        sleep 1
    fi

    if systemctl --user show-environment >/dev/null 2>&1; then
        guest_pass "systemd --user session available"
        return 0
    fi

    guest_fail "systemd --user session not available (XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR)"
    return 1
}

guest_assert_service_active() {
    local unit="${1:-thermalwriter.service}"
    if systemctl --user is-active --quiet "$unit"; then
        guest_pass "service active: $unit"
    else
        guest_fail "service not active: $unit"
        systemctl --user --no-pager --full status "$unit" || true
        journalctl --user -u "$unit" -n 50 --no-pager || true
        return 1
    fi
}

guest_assert_ctl_status() {
    local bin="${1:-thermalwriter}"
    local timeout_s="${2:-20}"
    guest_require_cmd "$bin" || return 1

    # Daemon registers D-Bus shortly after systemd marks the unit active.
    # Poll until status responds or timeout — avoids flaky ServiceUnknown races.
    local out="" attempt=0 start now
    start="$(date +%s)"
    while true; do
        attempt=$((attempt + 1))
        if out="$("$bin" ctl status 2>&1)"; then
            break
        fi
        now="$(date +%s)"
        if (( now - start >= timeout_s )); then
            guest_fail "ctl status failed after ${timeout_s}s (${attempt} attempts): $out"
            return 1
        fi
        sleep 0.5
    done
    if (( attempt > 1 )); then
        guest_info "ctl status ready after ${attempt} attempts"
    fi
    guest_info "ctl status:"
    printf '%s\n' "$out" | sed 's/^/      /'

    local key
    for key in active_layout connected mode resolution tick_rate; do
        if printf '%s\n' "$out" | grep -q "^${key}:"; then
            guest_pass "ctl status has $key"
        else
            guest_fail "ctl status missing $key"
        fi
    done

    # No hardware expected in clean VMs.
    if printf '%s\n' "$out" | grep -qi '^connected:[[:space:]]*false$'; then
        guest_pass "connected=false (no hardware)"
    elif printf '%s\n' "$out" | grep -qi '^connected:[[:space:]]*true$'; then
        guest_info "connected=true (hardware present — unexpected in clean VM but not a hard fail)"
        guest_pass "connected reported (true)"
    else
        guest_fail "connected field not true/false"
    fi
}

guest_assert_service_stays_up() {
    local seconds="${1:-30}"
    local unit="${2:-thermalwriter.service}"
    guest_log "waiting ${seconds}s to confirm service stays up"
    sleep "$seconds"
    if systemctl --user is-active --quiet "$unit"; then
        guest_pass "service still active after ${seconds}s"
    else
        guest_fail "service died within ${seconds}s"
        systemctl --user --no-pager --full status "$unit" || true
        return 1
    fi

    if thermalwriter ctl status >/dev/null 2>&1; then
        guest_pass "ctl status still responds after ${seconds}s"
    else
        guest_fail "ctl status stopped responding after ${seconds}s"
        return 1
    fi
}

guest_assert_udev_rule() {
    local rule="/etc/udev/rules.d/99-thermalwriter-rapl.rules"
    if [[ -f "$rule" ]]; then
        guest_pass "udev rule installed: $rule"
    else
        guest_fail "udev rule missing: $rule"
        return 1
    fi
}

guest_assert_unit_execstart() {
    local expected_bin="$1"
    local unit_path="${2:-$HOME/.config/systemd/user/thermalwriter.service}"
    if [[ ! -f "$unit_path" ]]; then
        guest_fail "unit file missing: $unit_path"
        return 1
    fi
    local line
    line="$(grep -E '^ExecStart=' "$unit_path" | head -1 || true)"
    guest_info "unit $line"
    # ExecStart="/path/thermalwriter" daemon  OR  ExecStart=/path/thermalwriter daemon
    if printf '%s\n' "$line" | grep -F "$expected_bin" >/dev/null; then
        guest_pass "ExecStart points at $expected_bin"
    else
        guest_fail "ExecStart does not contain $expected_bin (got: $line)"
        return 1
    fi
}

guest_cleanup_prior_install() {
    guest_log "cleaning prior install if present"
    local candidates=(
        "$HOME/thermalwriter-extract/packaging/uninstall.sh"
        "$HOME/thermalwriter-src/packaging/uninstall.sh"
    )
    local u
    for u in "${candidates[@]}"; do
        if [[ -x "$u" ]]; then
            # uninstall may fail if nothing installed
            bash "$u" || true
        fi
    done
    # Best-effort manual cleanup
    systemctl --user disable --now thermalwriter-tray.service 2>/dev/null || true
    systemctl --user disable --now thermalwriter.service 2>/dev/null || true
    rm -f "$HOME/.config/systemd/user/thermalwriter.service"
    rm -f "$HOME/.config/systemd/user/thermalwriter-tray.service"
    rm -f "$HOME/.config/autostart/thermalwriter-tray.desktop"
    systemctl --user daemon-reload 2>/dev/null || true
    rm -f "${CARGO_HOME:-$HOME/.cargo}/bin/thermalwriter"
    rm -f "${CARGO_HOME:-$HOME/.cargo}/bin/thermalwriter-tray"
    if [[ -f /etc/udev/rules.d/99-thermalwriter-rapl.rules ]]; then
        sudo rm -f /etc/udev/rules.d/99-thermalwriter-rapl.rules
        sudo udevadm control --reload-rules 2>/dev/null || true
    fi
}

guest_assert_tray_launch_path() {
    local unit="$HOME/.config/systemd/user/thermalwriter-tray.service"
    local autostart="$HOME/.config/autostart/thermalwriter-tray.desktop"
    local has_unit=0 has_autostart=0

    if [[ -f "$unit" ]]; then
        has_unit=1
    fi
    if [[ -f "$autostart" ]]; then
        has_autostart=1
    fi

    if [[ "$has_unit" -eq 1 && "$has_autostart" -eq 1 ]]; then
        guest_fail "tray has both systemd unit and XDG autostart (expected one path)"
        return 1
    fi
    if [[ "$has_unit" -eq 1 ]]; then
        guest_pass "tray enabled via systemd user unit"
        return 0
    fi
    if [[ "$has_autostart" -eq 1 ]]; then
        guest_pass "tray enabled via XDG autostart"
        return 0
    fi
    guest_fail "tray has neither systemd unit nor XDG autostart"
    return 1
}

guest_finish() {
    if [[ "${GUEST_FAIL:-0}" -ne 0 ]]; then
        printf 'RESULT: FAIL\n'
        exit 1
    fi
    printf 'RESULT: PASS\n'
    exit 0
}
