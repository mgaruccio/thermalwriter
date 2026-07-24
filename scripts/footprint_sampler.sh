#!/usr/bin/env bash
# footprint_sampler.sh — steady-state footprint of a running systemd user unit.
#
# Used for the cross-tool comparison in docs/comparison-methodology.md: every
# tool (thermalwriter and the Python alternatives) is launched inside its own
# systemd user unit, then measured by this script with identical math, so no
# tool's numbers depend on tool-specific instrumentation.
#
# Usage:
#   scripts/footprint_sampler.sh <user-unit-name> [warmup_s] [measure_s]
#
#   <user-unit-name>  e.g. thermalwriter.service or a systemd-run transient
#                     unit (footprint-trcc.service). Must already be running.
#   warmup_s          seconds to wait before measuring (default 30)
#   measure_s         measurement window (default 60)
#
# What it measures, over the whole cgroup (all processes in the unit,
# including short-lived children like forked sensor tools):
#   - cpu_seconds / cpu_pct_one_core: delta of cpu.stat usage_usec across the
#     measure window (kernel accounting; includes exited children)
#   - avg/peak RSS and PSS: sum of /proc/<pid>/smaps_rollup Rss:/Pss: across
#     every PID in cgroup.procs, sampled every SAMPLE_INTERVAL (default 0.5s).
#     PSS is the headline number for multi-process tools: shared pages are
#     divided among the processes that map them instead of double-counted.
#
# METRIC lines go to stdout (one per line); diagnostics to stderr.

set -euo pipefail

UNIT="${1:?usage: footprint_sampler.sh <user-unit> [warmup_s] [measure_s]}"
WARMUP_SECONDS="${2:-30}"
MEASURE_SECONDS="${3:-60}"
SAMPLE_INTERVAL="${SAMPLE_INTERVAL:-0.5}"

CGROUP_REL=$(systemctl --user show -P ControlGroup "$UNIT")
if [[ -z "$CGROUP_REL" ]]; then
  echo "!! unit $UNIT has no control group (not running?)" >&2
  exit 1
fi
CGROUP="/sys/fs/cgroup${CGROUP_REL}"
if [[ ! -d "$CGROUP" ]]; then
  echo "!! cgroup dir not found: $CGROUP" >&2
  exit 1
fi

cgroup_cpu_usec() {
  awk '/^usage_usec/ {print $2}' "$CGROUP/cpu.stat"
}

# Sum an smaps_rollup field (Rss/Pss, in KB) across every PID in the cgroup.
# PIDs may exit between listing and reading; missing files contribute 0.
tree_smaps_kb() {
  local field="$1" total=0 pid v
  while read -r pid; do
    v=$(awk -v f="^${field}:" '$0 ~ f {print $2}' "/proc/$pid/smaps_rollup" 2>/dev/null || true)
    total=$(( total + ${v:-0} ))
  done < "$CGROUP/cgroup.procs"
  echo "$total"
}

echo ">> unit=$UNIT cgroup=$CGROUP_REL" >&2
echo ">> warmup ${WARMUP_SECONDS}s..." >&2
sleep "$WARMUP_SECONDS"

cpu_start=$(cgroup_cpu_usec)
t_start=$(date +%s.%N)

samples=0
rss_sum=0; rss_peak=0
pss_sum=0; pss_peak=0
end_epoch=$(awk -v t="$t_start" -v m="$MEASURE_SECONDS" 'BEGIN{printf "%f", t + m}')

echo ">> measuring ${MEASURE_SECONDS}s at ${SAMPLE_INTERVAL}s intervals..." >&2
while awk -v now="$(date +%s.%N)" -v end="$end_epoch" 'BEGIN{exit !(now < end)}'; do
  rss=$(tree_smaps_kb Rss)
  pss=$(tree_smaps_kb Pss)
  samples=$(( samples + 1 ))
  rss_sum=$(( rss_sum + rss ));  (( rss > rss_peak )) && rss_peak=$rss
  pss_sum=$(( pss_sum + pss ));  (( pss > pss_peak )) && pss_peak=$pss
  sleep "$SAMPLE_INTERVAL"
done

cpu_end=$(cgroup_cpu_usec)
t_end=$(date +%s.%N)

if (( samples == 0 )); then
  echo "!! no samples collected" >&2
  exit 1
fi

wall=$(awk -v a="$t_start" -v b="$t_end" 'BEGIN{printf "%.3f", b - a}')
cpu_s=$(awk -v a="$cpu_start" -v b="$cpu_end" 'BEGIN{printf "%.3f", (b - a) / 1e6}')
cpu_pct=$(awk -v c="$cpu_s" -v w="$wall" 'BEGIN{printf "%.2f", 100 * c / w}')

echo "METRIC unit=$UNIT"
echo "METRIC samples=$samples"
echo "METRIC wall_seconds=$wall"
echo "METRIC cpu_seconds=$cpu_s"
echo "METRIC cpu_pct_one_core=$cpu_pct"
echo "METRIC avg_rss_kb=$(( rss_sum / samples ))"
echo "METRIC peak_rss_kb=$rss_peak"
echo "METRIC avg_pss_kb=$(( pss_sum / samples ))"
echo "METRIC peak_pss_kb=$pss_peak"
