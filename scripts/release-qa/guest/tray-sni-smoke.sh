#!/usr/bin/env bash
# Guest: register thermalwriter-tray against a real desktop SNI host.
#
# Usage:
#   tray-sni-smoke.sh gnome   # Ubuntu GNOME Shell + AppIndicator extension
#   tray-sni-smoke.sh kde     # KDE Plasma StatusNotifierWatcher (kded6)
#
# Expects thermalwriter-tray on PATH (or TRAY_BIN). Does not require the
# thermalwriter daemon — the tray starts fine while the daemon is offline.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common-assert.sh
source "$SCRIPT_DIR/common-assert.sh"

MODE="${1:-}"
case "$MODE" in
    gnome|kde) ;;
    *)
        echo "usage: $0 gnome|kde" >&2
        exit 2
        ;;
esac

TRAY_BIN="${TRAY_BIN:-$(command -v thermalwriter-tray || true)}"
[[ -n "$TRAY_BIN" && -x "$TRAY_BIN" ]] || {
    guest_fail "thermalwriter-tray not found (set TRAY_BIN=...)"
    guest_finish
}

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=${XDG_RUNTIME_DIR}/bus}"
export RUST_LOG="${RUST_LOG:-info}"

WORKDIR="${TMPDIR:-/tmp}/tw-tray-sni-$$"
mkdir -p "$WORKDIR"
XVFB_PID=""
HOST_PID=""
TRAY_PID=""

reap_pid() {
    local pid="${1:-}"
    [[ -n "$pid" ]] || return 0
    kill "$pid" 2>/dev/null || true
    # Bounded wait — gnome-shell/kded can ignore SIGTERM briefly.
    local i
    for i in 1 2 3 4 5 6 7 8 9 10; do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 0.2
    done
    kill -KILL "$pid" 2>/dev/null || true
}

cleanup() {
    reap_pid "${TRAY_PID:-}"
    TRAY_PID=""
    if [[ -n "${HOST_PID:-}" ]]; then
        pkill -P "$HOST_PID" 2>/dev/null || true
        reap_pid "$HOST_PID"
        HOST_PID=""
    fi
    reap_pid "${XVFB_PID:-}"
    XVFB_PID=""
    # Best-effort leftover cleanup for DE hosts we started.
    case "$MODE" in
        gnome) pkill -9 -f 'gnome-shell --headless' 2>/dev/null || true ;;
        kde)
            pkill -9 -x kded6 2>/dev/null || true
            pkill -9 -x Xvfb 2>/dev/null || true
            ;;
    esac
    pkill -9 -f "[/]thermalwriter-tray" 2>/dev/null || true
    rm -rf "$WORKDIR"
}
trap cleanup EXIT

watcher_present() {
    busctl --user status org.kde.StatusNotifierWatcher >/dev/null 2>&1
}

list_items() {
    local out
    out="$(busctl --user get-property org.kde.StatusNotifierWatcher \
        /StatusNotifierWatcher org.kde.StatusNotifierWatcher \
        RegisteredStatusNotifierItems 2>/dev/null || true)"
    printf '%s\n' "$out" | grep -oE 'org\.kde\.StatusNotifierItem-[^" ]+' || true
}

item_registered() {
    local want="StatusNotifierItem-${TRAY_PID}-"
    local items bus
    items="$(list_items)"
    if printf '%s\n' "$items" | grep -q "$want"; then
        guest_info "registered items: $(printf '%s' "$items" | tr '\n' ' ')"
        return 0
    fi
    bus="$(busctl --user list 2>/dev/null || true)"
    if printf '%s\n' "$bus" | grep -q "org.kde.${want}"; then
        guest_info "SNI name present on session bus for pid $TRAY_PID"
        printf '%s\n' "$bus" | grep 'StatusNotifierItem-' | sed 's/^/      /' || true
        return 0
    fi
    return 1
}

wait_for() {
    local desc="$1" seconds="$2"
    shift 2
    local start now
    start="$(date +%s)"
    while true; do
        if "$@"; then
            guest_pass "$desc"
            return 0
        fi
        now="$(date +%s)"
        if (( now - start >= seconds )); then
            guest_fail "$desc (timeout ${seconds}s)"
            return 1
        fi
        sleep 0.5
    done
}

process_alive() {
    kill -0 "$1" 2>/dev/null
}

start_gnome_host() {
    guest_log "starting GNOME Shell headless + AppIndicator extension"
    guest_require_cmd gnome-shell busctl || return 1

    # Ubuntu packages the extension as ubuntu-appindicators@ubuntu.com;
    # upstream id is appindicatorsupport@rgcjonas.gmail.com.
    local ext_id=""
    if [[ -d /usr/share/gnome-shell/extensions/ubuntu-appindicators@ubuntu.com ]]; then
        ext_id='ubuntu-appindicators@ubuntu.com'
    elif [[ -d /usr/share/gnome-shell/extensions/appindicatorsupport@rgcjonas.gmail.com ]]; then
        ext_id='appindicatorsupport@rgcjonas.gmail.com'
    else
        guest_fail "gnome-shell AppIndicator extension not installed under /usr/share/gnome-shell/extensions"
        return 1
    fi
    guest_info "extension id: $ext_id"

    # A stale marker disables all extensions for the session.
    rm -f "${XDG_RUNTIME_DIR}/gnome-shell-disable-extensions"
    pkill -f 'gnome-shell --headless' 2>/dev/null || true
    sleep 0.5

    if command -v gsettings >/dev/null 2>&1; then
        gsettings set org.gnome.shell disable-user-extensions false 2>/dev/null || true
        gsettings set org.gnome.shell enabled-extensions "['${ext_id}']" 2>/dev/null || true
    fi

    export XDG_CURRENT_DESKTOP=GNOME
    export XDG_SESSION_TYPE=wayland

    nohup gnome-shell --headless --wayland --mode=user \
        >"$WORKDIR/gnome-shell.log" 2>&1 &
    HOST_PID=$!
    guest_info "gnome-shell pid=$HOST_PID"

    if ! wait_for "GNOME StatusNotifierWatcher on session bus" 45 watcher_present; then
        guest_info "gnome-shell log tail:"
        tail -n 40 "$WORKDIR/gnome-shell.log" 2>/dev/null | sed 's/^/      /' || true
        return 1
    fi

    # Ensure the extension is enabled against the live shell.
    if command -v gnome-extensions >/dev/null 2>&1; then
        gnome-extensions enable "$ext_id" 2>/dev/null || true
    fi
    sleep 1
    return 0
}

start_kde_host() {
    guest_log "starting KDE Plasma StatusNotifierWatcher (kded6)"
    local kded=""
    for c in kded6 /usr/bin/kded6 /usr/lib/kf6/kded6; do
        if command -v "$c" >/dev/null 2>&1 || [[ -x "$c" ]]; then
            kded="$(command -v "$c" 2>/dev/null || echo "$c")"
            [[ -x "$kded" ]] && break
        fi
    done
    [[ -x "$kded" ]] || {
        guest_fail "kded6 not found (install plasma-workspace)"
        return 1
    }
    guest_info "using $kded"

    guest_require_cmd Xvfb busctl || return 1
    pkill -x kded6 2>/dev/null || true
    pkill -x Xvfb 2>/dev/null || true
    sleep 0.5

    # kded needs a real X display even headless; offscreen alone is not enough
    # for the statusnotifierwatcher module on Plasma 6.
    Xvfb :99 -screen 0 1024x768x24 >"$WORKDIR/xvfb.log" 2>&1 &
    XVFB_PID=$!
    sleep 0.5
    if ! kill -0 "$XVFB_PID" 2>/dev/null; then
        guest_fail "Xvfb failed to start"
        cat "$WORKDIR/xvfb.log" 2>/dev/null | sed 's/^/      /' || true
        return 1
    fi

    export DISPLAY=:99
    export QT_QPA_PLATFORM=xcb
    export XDG_CURRENT_DESKTOP=KDE
    export KDE_SESSION_VERSION=6

    nohup "$kded" >"$WORKDIR/kded.log" 2>&1 &
    HOST_PID=$!
    guest_info "kded pid=$HOST_PID"
    sleep 1

    if ! kill -0 "$HOST_PID" 2>/dev/null; then
        guest_fail "kded exited immediately"
        cat "$WORKDIR/kded.log" 2>/dev/null | sed 's/^/      /' || true
        return 1
    fi

    # Ensure the module is loaded (usually autoloads; force on demand).
    busctl --user call org.kde.kded6 /kded org.kde.kded6 loadModule s statusnotifierwatcher \
        >/dev/null 2>&1 || true

    if ! wait_for "KDE StatusNotifierWatcher on session bus" 45 watcher_present; then
        guest_info "kded log tail:"
        tail -n 40 "$WORKDIR/kded.log" 2>/dev/null | sed 's/^/      /' || true
        return 1
    fi
    return 0
}

guest_log "tray SNI smoke ($MODE) using $TRAY_BIN"
guest_ensure_user_systemd || guest_finish

if ! busctl --user status >/dev/null 2>&1; then
    guest_fail "no user D-Bus session (busctl --user status failed)"
    guest_finish
fi

case "$MODE" in
    gnome) start_gnome_host || guest_finish ;;
    kde) start_kde_host || guest_finish ;;
esac

pkill -f "[/]thermalwriter-tray" 2>/dev/null || true
sleep 0.5

guest_log "launching thermalwriter-tray"
nohup "$TRAY_BIN" >"$WORKDIR/tray.log" 2>&1 &
TRAY_PID=$!
guest_info "tray pid=$TRAY_PID"

if ! wait_for "tray process still running" 5 process_alive "$TRAY_PID"; then
    guest_info "tray log:"
    sed 's/^/      /' "$WORKDIR/tray.log" 2>/dev/null || true
    guest_finish
fi

if wait_for "tray registered StatusNotifierItem with $MODE host" 30 item_registered; then
    :
else
    guest_info "tray log:"
    sed 's/^/      /' "$WORKDIR/tray.log" 2>/dev/null || true
    guest_info "busctl StatusNotifier names:"
    busctl --user list 2>/dev/null | grep -i StatusNotifier | sed 's/^/      /' || true
    guest_finish
fi

if grep -q 'tray registered' "$WORKDIR/tray.log" 2>/dev/null; then
    guest_pass "tray log reports registered"
else
    guest_info "tray log missing explicit 'tray registered' line (SNI bus presence is enough)"
fi

kill -TERM "$TRAY_PID" 2>/dev/null || true
for _ in $(seq 1 20); do
    kill -0 "$TRAY_PID" 2>/dev/null || break
    sleep 0.25
done
if kill -0 "$TRAY_PID" 2>/dev/null; then
    guest_fail "tray did not exit on SIGTERM"
    kill -KILL "$TRAY_PID" 2>/dev/null || true
else
    guest_pass "tray exited cleanly on SIGTERM"
    TRAY_PID=""
fi

guest_finish
