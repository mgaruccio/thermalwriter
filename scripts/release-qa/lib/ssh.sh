#!/usr/bin/env bash
# SSH/SCP helpers for release-QA guests.
# shellcheck shell=bash

# shellcheck source=common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

QA_SSH_USER="${QA_SSH_USER:-qa}"
QA_SSH_OPTS=(
    -o StrictHostKeyChecking=accept-new
    -o UserKnownHostsFile="${THERMALWRITER_QA_KNOWN_HOSTS:-$(qa_default_vm_dir)/known_hosts}"
    -o ConnectTimeout=10
    -o ServerAliveInterval=15
    -o LogLevel=ERROR
)

if [[ -n "${THERMALWRITER_QA_SSH_KEY:-}" ]]; then
    QA_SSH_OPTS+=(-i "$THERMALWRITER_QA_SSH_KEY")
elif [[ -f "${HOME}/.ssh/id_ed25519" ]]; then
    QA_SSH_OPTS+=(-i "${HOME}/.ssh/id_ed25519")
elif [[ -f "${HOME}/.ssh/id_rsa" ]]; then
    QA_SSH_OPTS+=(-i "${HOME}/.ssh/id_rsa")
fi

qa_ssh() {
    local host="$1"
    shift
    ssh "${QA_SSH_OPTS[@]}" "${QA_SSH_USER}@${host}" "$@"
}

qa_scp_to() {
    local host="$1"
    local dest="$2"
    shift 2
    scp "${QA_SSH_OPTS[@]}" "$@" "${QA_SSH_USER}@${host}:${dest}"
}

qa_scp_from() {
    local host="$1"
    local remote="$2"
    local local_dest="$3"
    scp "${QA_SSH_OPTS[@]}" "${QA_SSH_USER}@${host}:${remote}" "$local_dest"
}

qa_wait_ssh() {
    local host="$1"
    local timeout_s="${2:-180}"
    local start now
    start="$(date +%s)"
    qa_log "waiting for ssh on ${QA_SSH_USER}@${host} (timeout ${timeout_s}s)"
    while true; do
        if qa_ssh "$host" 'true' 2>/dev/null; then
            qa_info "ssh ready"
            return 0
        fi
        now="$(date +%s)"
        if (( now - start >= timeout_s )); then
            qa_die "ssh not ready on $host after ${timeout_s}s"
        fi
        sleep 2
    done
}
