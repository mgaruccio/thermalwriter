#!/usr/bin/env bash
# autoresearch.sh — benchmark harness for memory-usage optimization.
#
# Builds and runs two companion benchmarks:
#   - memory_bench: measures RSS, peak RSS, allocation churn, live heap
#   - cpu_bench: measures uninstrumented fps and ms/frame
#
# METRIC lines go to stdout (one per line, parseable by autoresearch).
# Diagnostics go to stderr. Exit 0 on success, non-zero on failure.
#
# Env overrides:
#   MEMORY_BENCH_FRAMES (default 200) — measurement frames for memory_bench; must be >0
#   MEMORY_BENCH_WARMUP (default 50)  — warmup frames for memory_bench; may be 0
#   CPU_BENCH_FRAMES  (default 200)   — measurement frames for cpu_bench; must be >0
#   CPU_BENCH_WARMUP  (default 50)    — warmup frames for cpu_bench; may be 0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

require_non_negative_integer() {
    local name="$1"
    local value="$2"
    if [[ ! "$value" =~ ^[0-9]+$ ]]; then
        echo "!! invalid ${name}: ${value}" >&2
        exit 2
    fi
}

require_positive_integer() {
    local name="$1"
    local value="$2"
    require_non_negative_integer "$name" "$value"
    if [[ "$value" == "0" ]]; then
        echo "!! invalid ${name}: ${value}" >&2
        exit 2
    fi
}

MEMORY_FRAMES="${MEMORY_BENCH_FRAMES:-200}"
MEMORY_WARMUP="${MEMORY_BENCH_WARMUP:-50}"
CPU_FRAMES="${CPU_BENCH_FRAMES:-200}"
CPU_WARMUP="${CPU_BENCH_WARMUP:-50}"

require_positive_integer MEMORY_BENCH_FRAMES "$MEMORY_FRAMES"
require_non_negative_integer MEMORY_BENCH_WARMUP "$MEMORY_WARMUP"
require_positive_integer CPU_BENCH_FRAMES "$CPU_FRAMES"
require_non_negative_integer CPU_BENCH_WARMUP "$CPU_WARMUP"

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
echo ">> Building memory_bench and cpu_bench (release mode)..." >&2
if ! cargo build --release --example memory_bench --example cpu_bench >&2; then
    echo "!! cargo build failed" >&2
    exit 1
fi

MEMORY_BIN="./target/release/examples/memory_bench"
CPU_BIN="./target/release/examples/cpu_bench"

if [[ ! -x "$MEMORY_BIN" ]]; then
    echo "!! memory_bench binary not found: $MEMORY_BIN" >&2
    exit 1
fi
if [[ ! -x "$CPU_BIN" ]]; then
    echo "!! cpu_bench binary not found: $CPU_BIN" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

# memory_bench: stdout = METRIC lines, stderr = diagnostics.
echo ">> Running memory_bench (${MEMORY_WARMUP} warmup, ${MEMORY_FRAMES} measure)..." >&2
"$MEMORY_BIN" "$MEMORY_FRAMES" "$MEMORY_WARMUP"

# cpu_bench: stdout = METRIC lines, stderr = diagnostics.
echo ">> Running cpu_bench (${CPU_WARMUP} warmup, ${CPU_FRAMES} measure)..." >&2
"$CPU_BIN" "$CPU_FRAMES" "$CPU_WARMUP"

echo ">> All benchmarks completed successfully." >&2
exit 0
