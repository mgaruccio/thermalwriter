#!/usr/bin/env bash
# Create/start the Ubuntu 24.04 release-QA cloud VM.
# Usage: vm-ubuntu-up.sh [--reset]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/common.sh
source "$SCRIPT_DIR/../lib/common.sh"

qa_require_cmd qemu-system-x86_64 qemu-img
# cloud-localds from cloud-image-utils
if ! command -v cloud-localds >/dev/null 2>&1; then
    qa_die "cloud-localds not found — install cloud-image-utils (Arch: pacman -S cloud-image-utils)"
fi

VM_DIR="$(qa_default_vm_dir)/ubuntu-2404"
IMG_DIR="$(qa_default_vm_dir)/images"
SEED="$VM_DIR/seed.img"
DISK="$VM_DIR/disk.qcow2"
BASE_IMG="$IMG_DIR/ubuntu-24.04-server-cloudimg-amd64.img"
BASE_URL="${THERMALWRITER_QA_UBUNTU_IMAGE_URL:-https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-amd64.img}"
PIDFILE="$VM_DIR/qemu.pid"
MONITOR="$VM_DIR/monitor.sock"
SERIAL_LOG="$VM_DIR/serial.log"
SSH_PORT="${THERMALWRITER_QA_UBUNTU_SSH_PORT:-2222}"
RAM_MB="${THERMALWRITER_QA_UBUNTU_RAM_MB:-4096}"
CPUS="${THERMALWRITER_QA_UBUNTU_CPUS:-4}"
DISK_GB="${THERMALWRITER_QA_UBUNTU_DISK_GB:-20}"

RESET=0
if [[ "${1:-}" == "--reset" ]]; then
    RESET=1
fi

mkdir -p "$VM_DIR" "$IMG_DIR"
KNOWN_HOSTS="${THERMALWRITER_QA_KNOWN_HOSTS:-$(qa_default_vm_dir)/known_hosts}"
mkdir -p "$(dirname "$KNOWN_HOSTS")"

# --- base image ---
if [[ ! -f "$BASE_IMG" ]]; then
    qa_log "downloading Ubuntu 24.04 cloud image"
    tmp="${BASE_IMG}.partial"
    curl -fL --retry 3 --continue-at - -o "$tmp" "$BASE_URL"
    mv "$tmp" "$BASE_IMG"
fi

# --- ssh pubkey ---
PUBKEY_FILE="${THERMALWRITER_QA_SSH_PUBKEY:-}"
if [[ -z "$PUBKEY_FILE" ]]; then
    for c in "$HOME/.ssh/id_ed25519.pub" "$HOME/.ssh/id_rsa.pub"; do
        if [[ -f "$c" ]]; then
            PUBKEY_FILE="$c"
            break
        fi
    done
fi
[[ -n "$PUBKEY_FILE" && -f "$PUBKEY_FILE" ]] || qa_die "no SSH public key found (set THERMALWRITER_QA_SSH_PUBKEY)"
PUBKEY="$(tr -d '\n' <"$PUBKEY_FILE")"

stop_vm() {
    if [[ -f "$PIDFILE" ]]; then
        local pid
        pid="$(cat "$PIDFILE" 2>/dev/null || true)"
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            qa_log "stopping qemu pid $pid"
            kill "$pid" 2>/dev/null || true
            for _ in $(seq 1 20); do
                kill -0 "$pid" 2>/dev/null || break
                sleep 0.5
            done
            kill -9 "$pid" 2>/dev/null || true
        fi
        rm -f "$PIDFILE"
    fi
}

if [[ "$RESET" -eq 1 ]]; then
    stop_vm
    rm -f "$DISK" "$SEED"
    ssh-keygen -f "$KNOWN_HOSTS" -R "[127.0.0.1]:$SSH_PORT" >/dev/null 2>&1 || true
fi

# --- cloud-init seed ---
if [[ ! -f "$SEED" || "$RESET" -eq 1 ]]; then
    qa_log "building cloud-init seed"
    ud_src="$SCRIPT_DIR/../cloud-init/user-data.yaml"
    md_src="$SCRIPT_DIR/../cloud-init/meta-data"
    ud_tmp="$VM_DIR/user-data"
    md_tmp="$VM_DIR/meta-data"
    sed "s|__SSH_PUBKEY__|${PUBKEY}|" "$ud_src" >"$ud_tmp"
    # Unique instance-id on reset so cloud-init re-runs
    if [[ "$RESET" -eq 1 ]]; then
        printf 'instance-id: tw-qa-ubuntu-%s\nlocal-hostname: tw-qa-ubuntu\n' "$(date +%s)" >"$md_tmp"
    else
        cp "$md_src" "$md_tmp"
        # ensure hostname distinct
        printf 'instance-id: tw-qa-ubuntu-001\nlocal-hostname: tw-qa-ubuntu\n' >"$md_tmp"
    fi
    cloud-localds "$SEED" "$ud_tmp" "$md_tmp"
fi

# --- overlay disk ---
if [[ ! -f "$DISK" ]]; then
    qa_log "creating ${DISK_GB}G overlay disk"
    qemu-img create -f qcow2 -F qcow2 -b "$BASE_IMG" "$DISK" "${DISK_GB}G"
fi

# --- already running? ---
if [[ -f "$PIDFILE" ]]; then
    pid="$(cat "$PIDFILE" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        qa_log "ubuntu VM already running (pid $pid, ssh 127.0.0.1:${SSH_PORT})"
        printf '127.0.0.1\n'
        printf '%s\n' "$SSH_PORT" >"$VM_DIR/ssh_port"
        exit 0
    fi
    rm -f "$PIDFILE"
fi

# Firmware (optional OVMF — Ubuntu cloudimg boots on SeaBIOS too)
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
if [[ -n "$OVMF_CODE" ]]; then
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
    if [[ -n "$OVMF_VARS_SRC" ]]; then
        OVMF_VARS="$VM_DIR/OVMF_VARS.fd"
        if [[ ! -f "$OVMF_VARS" || "${RESET:-0}" -eq 1 ]]; then
            cp "$OVMF_VARS_SRC" "$OVMF_VARS"
        fi
        BIOS_ARGS=(
            -drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE"
            -drive "if=pflash,format=raw,file=$OVMF_VARS"
        )
    else
        BIOS_ARGS=(-drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE")
    fi
fi

qa_log "starting Ubuntu QA VM (ssh port $SSH_PORT, ${RAM_MB}MB RAM, ${CPUS} cpus)"
# hostfwd SSH; user networking needs no root
qemu-system-x86_64 \
    -name tw-qa-ubuntu \
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
qa_info "serial log: $SERIAL_LOG"
qa_info "ssh: ssh -p $SSH_PORT qa@127.0.0.1"

# Wait for ssh
# shellcheck source=../lib/ssh.sh
source "$SCRIPT_DIR/../lib/ssh.sh"
# Override host port via SSH config-ish: use ssh -p
qa_ssh() {
    local host="$1"
    shift
    ssh -p "$SSH_PORT" "${QA_SSH_OPTS[@]}" "${QA_SSH_USER}@${host}" "$@"
}
export -f qa_ssh 2>/dev/null || true

qa_log "waiting for cloud-init / ssh"
start="$(date +%s)"
timeout_s=300
while true; do
    if ssh -p "$SSH_PORT" "${QA_SSH_OPTS[@]}" qa@127.0.0.1 'cloud-init status --wait >/dev/null 2>&1 || true; true' 2>/dev/null; then
        # Confirm a real command works
        if ssh -p "$SSH_PORT" "${QA_SSH_OPTS[@]}" qa@127.0.0.1 'echo ok' 2>/dev/null | grep -q ok; then
            break
        fi
    fi
    now="$(date +%s)"
    if (( now - start >= timeout_s )); then
        qa_info "serial log tail:"
        tail -n 50 "$SERIAL_LOG" 2>/dev/null || true
        qa_die "Ubuntu VM ssh not ready after ${timeout_s}s"
    fi
    sleep 3
done

qa_log "Ubuntu VM ready on 127.0.0.1:$SSH_PORT"
printf '127.0.0.1:%s\n' "$SSH_PORT"
