#!/usr/bin/env bash
# Scenario profiling harness for the thermalwriter daemon.
#
# NOT a shipped subcommand — this is a dev-only script, invoked directly.
# Boots the daemon headlessly (THERMALWRITER_TRANSPORT=null, no cooler
# attached, under dbus-run-session so it never touches the live systemd
# service or the real USB device) once per scenario, captures a CPU
# flamegraph + RSS timeline (profiling build) and an allocation profile
# (dhat-heap build), and emits a machine-generated markdown summary.
#
# Usage:
#   scripts/profile.sh --list                 # show available scenarios
#   scripts/profile.sh <scenario>             # profile one scenario
#   scripts/profile.sh --all                  # curated ~12-scenario sweep
#
# Compare workflow for the criterion micro-benches (per-stage, cross-machine
# comparable) lives in docs/profiling.md — this script captures whole-daemon,
# machine-specific numbers instead.
#
# Env overrides:
#   WARMUP_SECONDS (default 10), MEASURE_SECONDS (default 60),
#   STARTUP_MEASURE_SECONDS (default 10), RSS_SAMPLE_INTERVAL (default 0.5)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

RESULTS_DIR="$ROOT/profiling-results"
PROFILE_BIN="$ROOT/target/profiling/thermalwriter"

WARMUP_SECONDS="${WARMUP_SECONDS:-10}"
MEASURE_SECONDS="${MEASURE_SECONDS:-60}"
STARTUP_MEASURE_SECONDS="${STARTUP_MEASURE_SECONDS:-10}"
RSS_SAMPLE_INTERVAL="${RSS_SAMPLE_INTERVAL:-0.5}"

# ---------------------------------------------------------------------------
# Scenario table — the curated ~12-scenario default sweep from the plan.
# Fields: default_layout|mode|bg_image|tick_rate|is_startup
# (xvfb-conky is special-cased below; its command references the scratch
# wrapper dir, which only exists once a scratch config dir is created.)
# ---------------------------------------------------------------------------
DEFAULT_SWEEP=(
  neon-dash-v2
  neon-dash
  arc-gauge
  cyber-grid
  system-stats
  neon-dash-v2-bg-off
  neon-dash-v2-15fps
  neon-dash-v2-60fps
  neon-dash-v2-bg-off-15fps
  neon-dash-v2-bg-off-60fps
  xvfb-conky
  startup
)

scenario_fields() {
  case "$1" in
    neon-dash-v2)                 echo "svg/neon-dash-v2.svg|svg|dark-gradient.png|2" ;;
    neon-dash)                    echo "svg/neon-dash.svg|svg|dark-gradient.png|2" ;;
    arc-gauge)                    echo "svg/arc-gauge.svg|svg|dark-gradient.png|2" ;;
    cyber-grid)                   echo "svg/cyber-grid.svg|svg|dark-gradient.png|2" ;;
    system-stats)                 echo "system-stats.html|html|dark-gradient.png|2" ;;
    neon-dash-v2-bg-off)          echo "svg/neon-dash-v2.svg|svg||2" ;;
    neon-dash-v2-15fps)           echo "svg/neon-dash-v2.svg|svg|dark-gradient.png|15" ;;
    neon-dash-v2-60fps)           echo "svg/neon-dash-v2.svg|svg|dark-gradient.png|60" ;;
    neon-dash-v2-bg-off-15fps)    echo "svg/neon-dash-v2.svg|svg||15" ;;
    neon-dash-v2-bg-off-60fps)    echo "svg/neon-dash-v2.svg|svg||60" ;;
    xvfb-conky)                   echo "svg/neon-dash-v2.svg|xvfb||15" ;;
    startup)                      echo "svg/neon-dash-v2.svg|svg|dark-gradient.png|2" ;;
    *) return 1 ;;
  esac
}

is_known_scenario() { scenario_fields "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------------------
# Preflight — fail fast with actionable messages. Xvfb/conky absence only
# skips the xvfb-conky scenario (soft), everything else is a hard failure.
# ---------------------------------------------------------------------------
HAVE_PERF=0
HAVE_INFERNO=0
HAVE_XVFB_CONKY=0

preflight() {
  local requested_scenarios=("$@")
  local fail=0

  if ! command -v dbus-run-session >/dev/null 2>&1; then
    echo "ERROR: dbus-run-session not found — install dbus (e.g. 'sudo pacman -S dbus' / 'sudo apt install dbus')." >&2
    fail=1
  fi

  if command -v perf >/dev/null 2>&1; then
    HAVE_PERF=1
    local paranoid
    paranoid=$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo 999)
    if [[ "$paranoid" -gt 2 ]]; then
      echo "ERROR: kernel.perf_event_paranoid=$paranoid (need <=2 for 'perf record -p'). Fix with:" >&2
      echo "  sudo sysctl kernel.perf_event_paranoid=2" >&2
      fail=1
    fi
  else
    echo "ERROR: 'perf' not found — required for CPU flamegraphs. Install your distro's perf package" >&2
    echo "  (e.g. 'sudo pacman -S perf', 'sudo apt install linux-tools-generic')." >&2
    fail=1
  fi

  if command -v inferno-collapse-perf >/dev/null 2>&1 && command -v inferno-flamegraph >/dev/null 2>&1; then
    HAVE_INFERNO=1
  else
    echo "ERROR: inferno tools not found — required to turn 'perf script' output into a flamegraph." >&2
    echo "  Install with: cargo install inferno" >&2
    fail=1
  fi

  if command -v Xvfb >/dev/null 2>&1 && command -v conky >/dev/null 2>&1; then
    HAVE_XVFB_CONKY=1
  else
    echo "NOTICE: Xvfb and/or conky not found — the xvfb-conky scenario will be skipped." >&2
  fi

  for s in "${requested_scenarios[@]}"; do
    if [[ "$s" == "xvfb-conky" && "$HAVE_XVFB_CONKY" -ne 1 ]]; then
      echo "ERROR: scenario 'xvfb-conky' explicitly requested but Xvfb/conky are missing." >&2
      fail=1
    fi
  done

  if [[ "$fail" -ne 0 ]]; then
    echo "" >&2
    echo "Preflight failed — fix the above and re-run." >&2
    exit 1
  fi
}

# ---------------------------------------------------------------------------
# /proc helpers
# ---------------------------------------------------------------------------

# Sum of utime+stime (in clock ticks) for a PID, robust to comm names
# containing spaces/parens (splits after the LAST ") " per proc(5)).
proc_cpu_ticks() {
  local pid="$1" stat rest
  stat=$(cat "/proc/$pid/stat" 2>/dev/null) || { echo 0; return; }
  rest=${stat##*) }
  local -a f
  read -ra f <<< "$rest"
  echo $(( ${f[11]:-0} + ${f[12]:-0} ))
}

proc_rss_kb() {
  local pid="$1"
  grep -m1 '^VmRSS:' "/proc/$pid/status" 2>/dev/null | awk '{print $2}' || echo 0
}

sample_rss() {
  local pid="$1" duration="$2" csv="$3"
  local elapsed=0
  local end
  end=$(awk -v d="$duration" 'BEGIN{print d}')
  local start_epoch
  start_epoch=$(date +%s.%N)
  while :; do
    local now
    now=$(date +%s.%N)
    elapsed=$(awk -v a="$now" -v b="$start_epoch" 'BEGIN{printf "%.2f", a-b}')
    if awk -v e="$elapsed" -v d="$end" 'BEGIN{exit !(e>=d)}'; then
      break
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
      break
    fi
    local rss
    rss=$(proc_rss_kb "$pid")
    echo "$elapsed,$rss" >> "$csv"
    sleep "$RSS_SAMPLE_INTERVAL"
  done
}

clk_tck() { getconf CLK_TCK 2>/dev/null || echo 100; }

# ---------------------------------------------------------------------------
# Config generation — one scratch XDG_CONFIG_HOME/XDG_RUNTIME_DIR per run so
# scenarios never race each other or the live daemon's runtime dir. The
# daemon seeds built-in layouts/backgrounds/wrappers into the scratch config
# dir itself on first boot (Config::load + seed_* run unconditionally at
# startup) — the harness only needs to write config.toml.
# ---------------------------------------------------------------------------
write_scenario_config() {
  local scenario="$1" cfg_home="$2"
  local fields layout mode bg_image tick_rate
  fields="$(scenario_fields "$scenario")"
  IFS='|' read -r layout mode bg_image tick_rate <<< "$fields"

  local twdir="$cfg_home/thermalwriter"
  mkdir -p "$twdir"

  local xvfb_cmd=""
  if [[ "$scenario" == "xvfb-conky" ]]; then
    xvfb_cmd="conky -c $twdir/wrappers/conky-480.conf"
  fi

  {
    echo "[display]"
    echo "tick_rate = $tick_rate"
    echo "default_layout = \"$layout\""
    echo "jpeg_quality = 85"
    echo "rotation = 180"
    echo "mode = \"$mode\""
    echo
    echo "[sensors]"
    echo "poll_interval_ms = 1000"
    echo
    echo "[background]"
    if [[ -n "$bg_image" ]]; then
      echo "image = \"$bg_image\""
    fi
    echo
    echo "[xvfb]"
    echo "command = \"$xvfb_cmd\""
    echo "tick_rate = 15"
  } > "$twdir/config.toml"
}

# ---------------------------------------------------------------------------
# One scenario, one pass ("cpu": profiling build -> flamegraph + RSS timeline;
# "dhat": dhat-heap build -> allocation profile). One fresh daemon process per
# call, per the plan's cross-scenario-contamination decision.
# ---------------------------------------------------------------------------
run_pass() {
  local scenario="$1" bin="$2" pass="$3"
  local out_dir="$RESULTS_DIR/$scenario"
  mkdir -p "$out_dir"

  local scratch cfg_home run_home
  scratch=$(mktemp -d)
  cfg_home="$scratch/config"
  run_home="$scratch/run"
  mkdir -p "$cfg_home" "$run_home"

  write_scenario_config "$scenario" "$cfg_home"

  local warmup measure
  if [[ "$scenario" == "startup" ]]; then
    warmup=0
    measure="$STARTUP_MEASURE_SECONDS"
  else
    warmup="$WARMUP_SECONDS"
    measure="$MEASURE_SECONDS"
  fi

  local log_file="$out_dir/${pass}_daemon.log"
  echo ">> [$scenario/$pass] launching (warmup=${warmup}s, measure=${measure}s)"

  ( cd "$scratch" && XDG_CONFIG_HOME="$cfg_home" XDG_RUNTIME_DIR="$run_home" \
      THERMALWRITER_TRANSPORT=null RUST_LOG=info \
      dbus-run-session -- "$bin" daemon >"$log_file" 2>&1 ) &
  local wrapper_pid=$!

  # Anchored match: the dbus-run-session wrapper's own cmdline also contains
  # "$bin daemon" as a substring, so an unanchored pgrep would match both.
  local pid=""
  for _ in $(seq 1 50); do
    pid=$(pgrep -f "^$bin daemon\$" 2>/dev/null | head -1 || true)
    [[ -n "$pid" ]] && break
    sleep 0.1
  done
  if [[ -z "$pid" ]]; then
    echo "!! [$scenario/$pass] daemon did not start within 5s (see $log_file)" >&2
    wait "$wrapper_pid" 2>/dev/null || true
    rm -rf "$scratch"
    return 1
  fi

  local start_ticks
  start_ticks=$(proc_cpu_ticks "$pid")

  if [[ "$warmup" -gt 0 ]]; then
    sleep "$warmup"
  fi

  if [[ "$pass" == "cpu" ]]; then
    local rss_csv="$out_dir/rss_timeline.csv"
    echo "elapsed_seconds,rss_kb" > "$rss_csv"
    sample_rss "$pid" "$measure" "$rss_csv" &
    local rss_pid=$!

    local perf_data="$out_dir/perf.data"
    if [[ "$HAVE_PERF" == 1 ]]; then
      perf record --call-graph dwarf -p "$pid" -o "$perf_data" -- sleep "$measure" 2>>"$log_file" || true
    else
      sleep "$measure"
    fi
    wait "$rss_pid" 2>/dev/null || true
  else
    sleep "$measure"
  fi

  local end_ticks
  end_ticks=$(proc_cpu_ticks "$pid")

  kill -TERM "$pid" 2>/dev/null || true
  wait "$wrapper_pid" 2>/dev/null || true

  local frames_sent
  frames_sent=$(grep -o 'NullTransport closed: [0-9]* frames sent' "$log_file" | grep -o '[0-9]*' | head -1 || true)
  frames_sent="${frames_sent:-0}"

  if [[ "$pass" == "cpu" ]]; then
    local tck cpu_seconds cpu_per_frame_ms
    tck=$(clk_tck)
    cpu_seconds=$(awk -v s="$start_ticks" -v e="$end_ticks" -v t="$tck" 'BEGIN{printf "%.4f", (e-s)/t}')
    if [[ "$frames_sent" -gt 0 ]]; then
      cpu_per_frame_ms=$(awk -v c="$cpu_seconds" -v f="$frames_sent" 'BEGIN{printf "%.3f", (c*1000)/f}')
    else
      cpu_per_frame_ms="n/a"
    fi
    {
      echo "cpu_seconds=$cpu_seconds"
      echo "frames_sent=$frames_sent"
      echo "cpu_per_frame_ms=$cpu_per_frame_ms"
    } > "$out_dir/cpu_metrics.txt"

    if [[ "$HAVE_PERF" == 1 && "$HAVE_INFERNO" == 1 && -f "$perf_data" ]]; then
      perf script -i "$perf_data" 2>>"$log_file" | inferno-collapse-perf 2>>"$log_file" | inferno-flamegraph > "$out_dir/flamegraph.svg" 2>>"$log_file" || \
        echo "!! [$scenario/cpu] flamegraph generation failed (see $log_file)" >&2
    fi
  else
    if [[ -f "$scratch/dhat-heap.json" ]]; then
      mv "$scratch/dhat-heap.json" "$out_dir/dhat-heap.json"
    fi
    # dhat-heap.json has no flat "total bytes" field (it's a per-callsite
    # table under "pps") — the aggregate totals are on dhat's own stderr
    # summary, which we've already captured in $log_file since it's written
    # on Profiler drop, right after our SIGTERM.
    {
      echo "frames_sent=$frames_sent"
      local total_line gmax_line
      total_line=$(grep -m1 'dhat: Total:' "$log_file" || true)
      gmax_line=$(grep -m1 'dhat: At t-gmax:' "$log_file" || true)
      if [[ -n "$total_line" ]]; then
        echo "total_bytes_allocated=$(echo "$total_line" | grep -oE '[0-9,]+ bytes' | tr -d ', bytes')"
        echo "total_blocks_allocated=$(echo "$total_line" | grep -oE '[0-9,]+ blocks' | tr -d ', blocks')"
      fi
      if [[ -n "$gmax_line" ]]; then
        echo "gmax_bytes=$(echo "$gmax_line" | grep -oE '[0-9,]+ bytes' | tr -d ', bytes')"
      fi
    } > "$out_dir/dhat_metrics.txt"
  fi

  rm -rf "$scratch"
}

# ---------------------------------------------------------------------------
# Summary emitter
# ---------------------------------------------------------------------------
emit_summary() {
  local scenarios=("$@")
  local summary="$RESULTS_DIR/summary.md"
  {
    echo "# Profiling summary"
    echo
    echo "- commit: $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
    echo "- date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "- kernel: $(uname -r)"
    echo "- cpu: $(awk -F': ' '/model name/{print $2; exit}' /proc/cpuinfo 2>/dev/null || echo unknown)"
    echo "- build profile: profiling (CPU/RSS), profiling+dhat-heap (allocations)"
    echo
    echo "| scenario | cpu/frame (ms) | frames sent | avg RSS (KB) | peak RSS (KB) | total allocated (bytes) |"
    echo "|---|---|---|---|---|---|"
    for s in "${scenarios[@]}"; do
      local out_dir="$RESULTS_DIR/$s"
      local cpu_per_frame="n/a" frames="n/a" avg_rss="n/a" peak_rss="n/a" total_bytes="n/a"
      [[ -f "$out_dir/cpu_metrics.txt" ]] && cpu_per_frame=$(awk -F= '/cpu_per_frame_ms/{print $2}' "$out_dir/cpu_metrics.txt")
      [[ -f "$out_dir/cpu_metrics.txt" ]] && frames=$(awk -F= '/frames_sent/{print $2}' "$out_dir/cpu_metrics.txt")
      if [[ -f "$out_dir/rss_timeline.csv" ]]; then
        avg_rss=$(awk -F, 'NR>1{s+=$2;n++} END{if(n>0) printf "%.0f", s/n; else print "n/a"}' "$out_dir/rss_timeline.csv")
        peak_rss=$(awk -F, 'NR>1{if($2>m)m=$2} END{print (m>0)?m:"n/a"}' "$out_dir/rss_timeline.csv")
      fi
      [[ -f "$out_dir/dhat_metrics.txt" ]] && total_bytes=$(awk -F= '/total_bytes_allocated/{print $2}' "$out_dir/dhat_metrics.txt")
      echo "| $s | $cpu_per_frame | $frames | $avg_rss | $peak_rss | $total_bytes |"
    done
  } > "$summary"
  echo "Summary written to $summary"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
usage() {
  cat <<EOF
Usage: $(basename "$0") <scenario> | --all | --list

Scenarios:
$(for s in "${DEFAULT_SWEEP[@]}"; do echo "  $s"; done)
EOF
}

main() {
  if [[ $# -eq 0 || "$1" == "-h" || "$1" == "--help" ]]; then
    usage
    exit 0
  fi

  if [[ "$1" == "--list" ]]; then
    for s in "${DEFAULT_SWEEP[@]}"; do echo "$s"; done
    exit 0
  fi

  local scenarios=()
  if [[ "$1" == "--all" ]]; then
    scenarios=("${DEFAULT_SWEEP[@]}")
  else
    if ! is_known_scenario "$1"; then
      echo "Unknown scenario: $1" >&2
      usage >&2
      exit 1
    fi
    scenarios=("$1")
  fi

  preflight "${scenarios[@]}"

  # Drop xvfb-conky from an --all sweep if binaries are missing (soft skip);
  # a single explicit "xvfb-conky" request without binaries already failed
  # preflight above.
  local run_list=()
  for s in "${scenarios[@]}"; do
    if [[ "$s" == "xvfb-conky" && "$HAVE_XVFB_CONKY" -ne 1 ]]; then
      echo "NOTICE: skipping xvfb-conky (Xvfb/conky not found)"
      continue
    fi
    run_list+=("$s")
  done

  mkdir -p "$RESULTS_DIR"

  echo "== Building profiling profile (CPU + RSS pass) =="
  cargo build --profile profiling

  for s in "${run_list[@]}"; do
    run_pass "$s" "$PROFILE_BIN" "cpu"
  done

  echo "== Building profiling profile + dhat-heap (allocation pass) =="
  cargo build --profile profiling --features dhat-heap

  for s in "${run_list[@]}"; do
    run_pass "$s" "$PROFILE_BIN" "dhat"
  done

  emit_summary "${run_list[@]}"
}

main "$@"
