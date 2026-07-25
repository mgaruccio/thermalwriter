#!/usr/bin/env bash
# Create/start the Arch Linux release-QA cloud VM.
# Usage: vm-arch-up.sh [--reset]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/common.sh
source "$SCRIPT_DIR/../lib/common.sh"

qa_require_cmd qemu-system-x86_64 qemu-img
command -v cloud-localds >/dev/null 2>&1 || qa_die "cloud-localds not found — install cloud-image-utils"

VM_DIR="$(qa_default_vm_dir)/arch"
IMG_DIR="$(qa_default_vm_dir)/images"
SEED="$VM_DIR/seed.img"
DISK="$VM_DIR/disk.qcow2"
# Official Arch cloud image (qcow2)
BASE_IMG="$IMG_DIR/Arch-Linux-x86_64-cloudimg.qcow2"
BASE_URL="${THERMALWRITER_QA_ARCH_IMAGE_URL:-https://geo.mirror.pkgbuild.com/images/latest/Arch-Linux-x86_64-cloudimg.qcow2}"
PIDFILE="$VM_DIR/qemu.pid"
MONITOR="$VM_DIR/monitor.sock"
SERIAL_LOG="$VM_DIR/serial.log"
SSH_PORT="${THERMALWRITER_QA_ARCH_SSH_PORT:-2223}"
RAM_MB="${THERMALWRITER_QA_ARCH_RAM_MB:-8192}"
CPUS="${THERMALWRITER_QA_ARCH_CPUS:-6}"
DISK_GB="${THERMALWRITER_QA_ARCH_DISK_GB:-25}"

RESET=0
[[ "${1:-}" == "--reset" ]] && RESET=1

mkdir -p "$VM_DIR" "$IMG_DIR"

if [[ ! -f "$BASE_IMG" ]]; then
    qa_log "downloading Arch cloud image"
    tmp="${BASE_IMG}.partial"
    curl -fL --retry 3 --continue-at - -o "$tmp" "$BASE_URL"
    mv "$tmp" "$BASE_IMG"
fi

PUBKEY_FILE="${THERMALWRITER_QA_SSH_PUBKEY:-}"
if [[ -z "$PUBKEY_FILE" ]]; then
    for c in "$HOME/.ssh/id_ed25519.pub" "$HOME/.ssh/id_rsa.pub"; do
        [[ -f "$c" ]] && PUBKEY_FILE="$c" && break
    done
fi
[[ -n "$PUBKEY_FILE" && -f "$PUBKEY_FILE" ]] || qa_die "no SSH public key found"
PUBKEY="$(tr -d '\n' <"$PUBKEY_FILE")"

stop_vm() {
    if [[ -f "$PIDFILE" ]]; then
        local pid
        pid="$(cat "$PIDFILE" 2>/dev/null || true)"
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            qa_log "stopping qemu pid $pid"
            kill "$pid" 2>/dev/null || true
            sleep 2
            kill -9 "$pid" 2>/dev/null || true
        fi
        rm -f "$PIDFILE"
    fi
}

[[ "$RESET" -eq 1 ]] && stop_vm && rm -f "$DISK" "$SEED"

# Arch cloud image uses different defaults; build a dedicated user-data.
if [[ ! -f "$SEED" || "$RESET" -eq 1 ]]; then
    qa_log "building Arch cloud-init seed"
    ud_tmp="$VM_DIR/user-data"
    md_tmp="$VM_DIR/meta-data"
    cat >"$ud_tmp" <<EOF
#cloud-config
hostname: tw-qa-arch
users:
  - name: qa
    gecos: thermalwriter QA
    groups: [wheel]
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    lock_passwd: true
    ssh_authorized_keys:
      - ${PUBKEY}
package_update: true
packages:
  - git
  - base-devel
  - pkgconf
  - qemu-guest-agent
  - xorg-server-xvfb
  - fuse2
  - curl
  - fribidi
  - fontconfig
  - harfbuzz
  - gtk3
  - webkit2gtk-4.1
  - libsoup3
ssh_pwauth: false
runcmd:
  - [ loginctl, enable-linger, qa ]
  - [ systemctl, enable, --now, qemu-guest-agent ]
  - [ mkdir, -p, /run/user/1000 ]
  - [ chown, qa:qa, /run/user/1000 ]
  - [ chmod, 700, /run/user/1000 ]
EOF
    printf 'instance-id: tw-qa-arch-%s\nlocal-hostname: tw-qa-arch\n' "$(date +%s)" >"$md_tmp"
    cloud-localds "$SEED" "$ud_tmp" "$md_tmp"
fi

if [[ ! -f "$DISK" ]]; then
    qa_log "creating ${DISK_GB}G overlay disk"
    qemu-img create -f qcow2 -F qcow2 -b "$BASE_IMG" "$DISK" "${DISK_GB}G"
fi

if [[ -f "$PIDFILE" ]]; then
    pid="$(cat "$PIDFILE" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        qa_log "arch VM already running (pid $pid, ssh 127.0.0.1:${SSH_PORT})"
        printf '%s\n' "$SSH_PORT" >"$VM_DIR/ssh_port"
        printf '127.0.0.1:%s\n' "$SSH_PORT"
        exit 0
    fi
    rm -f "$PIDFILE"
fi

# UEFI firmware (Arch cloud images need OVMF)
BIOS_ARGS=()
OVMF_CODE=""
for ovmf in \
    /usr/share/edk2/x64/OVMF_CODE.4m.fd \
    /usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd \
    /usr/share/edk2/x64/OVMF_CODE.fd \
    /usr/share/edk2-ovmf/x64/OVMF_CODE.fd \
    /usr/share/OVMF/OVMF_CODE_4M.fd \
    /usr/share/OVMF/OVMF_CODE.fd
do
    if [[ -f "$ovmf" ]]; then
        OVMF_CODE="$ovmf"
        break
    fi
done
[[ -n "$OVMF_CODE" ]] || qa_die "OVMF firmware not found (install edk2-ovmf / ovmf)"

# Writable VARS copy (required for UEFI boot)
OVMF_VARS_SRC=""
case "$OVMF_CODE" in
    *.4m.fd|*_4M.fd)
        for v in \
            "$(dirname "$OVMF_CODE")/OVMF_VARS.4m.fd" \
            "$(dirname "$OVMF_CODE")/OVMF_VARS_4M.fd" \
            /usr/share/OVMF/OVMF_VARS_4M.fd
        do
            [[ -f "$v" ]] && OVMF_VARS_SRC="$v" && break
        done
        ;;
    *)
        for v in \
            "$(dirname "$OVMF_CODE")/OVMF_VARS.fd" \
            /usr/share/OVMF/OVMF_VARS.fd
        do
            [[ -f "$v" ]] && OVMF_VARS_SRC="$v" && break
        done
        ;;
esac
[[ -n "$OVMF_VARS_SRC" ]] || qa_die "OVMF_VARS not found next to $OVMF_CODE"
OVMF_VARS="$VM_DIR/OVMF_VARS.fd"
if [[ ! -f "$OVMF_VARS" || "${RESET:-0}" -eq 1 ]]; then
    cp "$OVMF_VARS_SRC" "$OVMF_VARS"
fi
BIOS_ARGS=(
    -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE"
    -drive "if=pflash,format=raw,file=$OVMF_VARS"
)

qa_log "starting Arch QA VM (ssh port $SSH_PORT)"
qemu-system-x86_64 \
    -name tw-qa-arch \
    -machine q35,accel=kvm \
    -cpu host \
    -smp "$CPUS" \
    -m "$RAM_MB" \
    "${BIOS_ARGS[@]}" \
    -drive "file=$DISK,if=virtio,format=qcow2,discard=unmap,detect-zeroes=unmap" \
    -drive "file=$SEED,if=virtio,format=raw" \
    -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:${SSH_PORT}-:22" \
    -device virtio-net-pci,netdev=net0 \
    -device virtio-balloon \
    -display none \
    -serial "file:$SERIAL_LOG" \
    -daemonize \
    -pidfile "$PIDFILE" \
    -monitor "unix:$MONITOR,server,nowait"

printf '%s\n' "$SSH_PORT" >"$VM_DIR/ssh_port"

# shellcheck source=../lib/ssh.sh
source "$SCRIPT_DIR/../lib/ssh.sh"

qa_log "waiting for cloud-init / ssh / pacman unlock"
start="$(date +%s)"
timeout_s=600
while true; do
    if ssh -p "$SSH_PORT" "${QA_SSH_OPTS[@]}" qa@127.0.0.1         'cloud-init status --wait >/dev/null 2>&1 || true
         # wait briefly if pacman still locked after cloud-init
         for i in 1 2 3 4 5 6 7 8 9 10; do
             [[ -e /var/lib/pacman/db.lck ]] || break
             sleep 1
         done
         [[ ! -e /var/lib/pacman/db.lck ]] && echo ok' 2>/dev/null | grep -q ok; then
        break
    fi
    now="$(date +%s)"
    if (( now - start >= timeout_s )); then
        qa_info "serial log tail:"
        tail -n 80 "$SERIAL_LOG" 2>/dev/null || true
        qa_die "Arch VM ssh/cloud-init not ready after ${timeout_s}s"
    fi
    sleep 3
done

qa_log "Arch VM ready on 127.0.0.1:$SSH_PORT"
printf '127.0.0.1:%s\n' "$SSH_PORT"
