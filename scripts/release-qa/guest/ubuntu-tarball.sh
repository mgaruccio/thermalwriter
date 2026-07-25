#!/usr/bin/env bash
# Ubuntu guest: install from release tarball + custom CARGO_HOME check.
# Expects artifacts already on the guest under ~/qa-artifacts/
# Usage: ubuntu-tarball.sh <tag|version>
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common-assert.sh
source "$SCRIPT_DIR/common-assert.sh"

raw="${1:-}"
[[ -n "$raw" ]] || { echo "usage: $0 <tag|version>" >&2; exit 2; }
if [[ "$raw" == v* ]]; then
    TAG="$raw"
    VERSION="${raw#v}"
else
    VERSION="$raw"
    TAG="v${raw}"
fi

ART_DIR="${QA_ARTIFACTS_DIR:-$HOME/qa-artifacts}"
TARBALL="$ART_DIR/thermalwriter-${TAG}-x86_64-unknown-linux-gnu.tar.gz"
EXTRACT="$HOME/thermalwriter-extract"

guest_log "Ubuntu tarball install QA for $TAG"
guest_ensure_user_systemd || guest_finish
guest_cleanup_prior_install

[[ -f "$TARBALL" ]] || { guest_fail "tarball missing: $TARBALL"; guest_finish; }

rm -rf "$EXTRACT"
mkdir -p "$EXTRACT"
tar -xzf "$TARBALL" -C "$EXTRACT" --strip-components=1
chmod +x "$EXTRACT/packaging/install.sh" "$EXTRACT/packaging/uninstall.sh" "$EXTRACT/bin/thermalwriter"

# --- default CARGO_HOME install ---
guest_log "running packaging/install.sh (default CARGO_HOME)"
(
    cd "$EXTRACT"
    ./packaging/install.sh
)

DEFAULT_BIN="${CARGO_HOME:-$HOME/.cargo}/bin/thermalwriter"
if [[ -x "$DEFAULT_BIN" ]]; then
    guest_pass "binary installed at $DEFAULT_BIN"
else
    guest_fail "binary missing at $DEFAULT_BIN"
fi

# Ensure PATH sees it for subsequent ctl calls
export PATH="$(dirname "$DEFAULT_BIN"):$PATH"

guest_assert_unit_execstart "$DEFAULT_BIN" || true
guest_assert_service_active || true
guest_assert_ctl_status thermalwriter || true
guest_assert_udev_rule || true
guest_assert_service_stays_up 30 || true

# --- custom CARGO_HOME reinstall ---
guest_log "reinstall with custom CARGO_HOME"
CUSTOM_HOME="$HOME/tw-custom-cargo"
rm -rf "$CUSTOM_HOME"
# Stop default install first
if [[ -x "$EXTRACT/packaging/uninstall.sh" ]]; then
    bash "$EXTRACT/packaging/uninstall.sh" || true
fi
# uninstall uses CARGO_HOME for binary path — also wipe default
rm -f "$HOME/.cargo/bin/thermalwriter"

(
    cd "$EXTRACT"
    export CARGO_HOME="$CUSTOM_HOME"
    # install.sh reads CARGO_HOME for destination
    ./packaging/install.sh
)

CUSTOM_BIN="$CUSTOM_HOME/bin/thermalwriter"
export PATH="$(dirname "$CUSTOM_BIN"):$PATH"

if [[ -x "$CUSTOM_BIN" ]]; then
    guest_pass "custom CARGO_HOME binary at $CUSTOM_BIN"
else
    guest_fail "custom CARGO_HOME binary missing"
fi

guest_assert_unit_execstart "$CUSTOM_BIN" || true
guest_assert_service_active || true
guest_assert_ctl_status thermalwriter || true

# Leave a working default-ish install for any follow-on GUI tests:
# reinstall to default location.
guest_log "restoring default CARGO_HOME install for follow-on tests"
bash "$EXTRACT/packaging/uninstall.sh" || true
rm -rf "$CUSTOM_HOME"
(
    cd "$EXTRACT"
    unset CARGO_HOME
    ./packaging/install.sh
)
export PATH="$HOME/.cargo/bin:$PATH"

guest_finish
