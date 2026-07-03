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
#   STARTUP_MEASURE_SECONDS (default 10), RSS_SAMPLE_INTERVAL (default 0.5),
#   SHUTDOWN_TIMEOUT_SECONDS (default 15)

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
SHUTDOWN_TIMEOUT_SECONDS="${SHUTDOWN_TIMEOUT_SECONDS:-15}"

# ---------------------------------------------------------------------------
# Cleanup + signal handling. Every in-flight resource a running scenario
# owns (scratch dir, dbus-run-session wrapper, daemon PID, RSS sampler) is
# tracked in these globals as run_pass acquires it, and cleared once run_pass
# has handled it itself — so a signal arriving between scenarios never kills
# an unrelated/recycled PID. Without this, Ctrl-C during a measurement
# window was observed to leave the daemon+wrapper running: every command in
# run_pass is `|| true`-guarded (by design, so one scenario's failure doesn't
# abort the whole sweep), which also swallows a SIGINT-caused nonzero exit —
# the loop just moved on to the next scenario. Explicit traps + `exit` make
# signals actually terminate the script.
# ---------------------------------------------------------------------------
CURRENT_SCRATCH=""
CURRENT_WRAPPER_PID=""
CURRENT_DAEMON_PID=""
CURRENT_RSS_PID=""
LOCK_DIR=""

cleanup_children() {
  # Best-effort, idempotent — safe to call more than once (e.g. from both an
  # INT/TERM trap's exit and the subsequent EXIT trap).
  if [[ -n "$CURRENT_RSS_PID" ]]; then
    kill -TERM "$CURRENT_RSS_PID" 2>/dev/null || true
    CURRENT_RSS_PID=""
  fi
  if [[ -n "$CURRENT_DAEMON_PID" ]]; then
    # SIGTERM (never SIGKILL) even during cleanup — gives dhat a chance to
    # write its allocation report on Profiler drop if a dhat-heap pass was
    # interrupted mid-window.
    kill -TERM "$CURRENT_DAEMON_PID" 2>/dev/null || true
    CURRENT_DAEMON_PID=""
  fi
  if [[ -n "$CURRENT_WRAPPER_PID" ]]; then
    kill -TERM "$CURRENT_WRAPPER_PID" 2>/dev/null || true
    wait "$CURRENT_WRAPPER_PID" 2>/dev/null || true
    CURRENT_WRAPPER_PID=""
  fi
  if [[ -n "$CURRENT_SCRATCH" && -d "$CURRENT_SCRATCH" ]]; then
    rm -rf "$CURRENT_SCRATCH"
    CURRENT_SCRATCH=""
  fi
  if [[ -n "$LOCK_DIR" && -d "$LOCK_DIR" ]]; then
    rm -rf "$LOCK_DIR" 2>/dev/null || true
    LOCK_DIR=""
  fi
}

on_exit() {
  local code=$?
  cleanup_children
  exit "$code"
}

on_interrupt() {
  echo "" >&2
  echo ">> Interrupted — terminating in-flight scenario and cleaning up." >&2
  exit 130
}

on_terminate() {
  echo "" >&2
  echo ">> Terminated — cleaning up." >&2
  exit 143
}

trap on_exit EXIT
trap on_interrupt INT
trap on_terminate TERM

# Refuse concurrent invocations: two harness runs would both `pgrep -f` the
# same binary path and could grab each other's daemon PID (see run_pass).
# A plain `mkdir` is atomic across processes, so this doubles as the lock.
acquire_lock() {
  LOCK_DIR="$ROOT/.profiling-harness.lock"
  if ! mkdir "$LOCK_DIR" 2>/dev/null; then
    echo "ERROR: another scripts/profile.sh run appears to be in progress" >&2
    echo "  (lock dir exists: $LOCK_DIR — remove it if this is stale)." >&2
    LOCK_DIR="" # not ours — don't let cleanup remove someone else's lock
    exit 1
  fi
  echo "$$" > "$LOCK_DIR/pid" 2>/dev/null || true
}

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

# Wall-clock epoch (fractional seconds) a PID started, from /proc/<pid>/stat's
# starttime (clock ticks since boot, field 22) + /proc/stat's btime (boot
# time, epoch seconds). Used for time-to-first-frame on the "startup"
# scenario — more precise than approximating "spawn time" as "whenever the
# harness's pgrep polling loop happened to notice the PID".
proc_start_epoch() {
  local pid="$1" stat rest tck btime
  stat=$(cat "/proc/$pid/stat" 2>/dev/null) || { echo ""; return; }
  rest=${stat##*) }
  local -a f
  read -ra f <<< "$rest"
  local starttime_ticks="${f[19]:-0}"
  tck=$(clk_tck)
  btime=$(awk '/^btime/{print $2; exit}' /proc/stat 2>/dev/null)
  if [[ -z "$btime" ]]; then
    echo ""
    return
  fi
  awk -v b="$btime" -v s="$starttime_ticks" -v t="$tck" 'BEGIN{printf "%.4f", b + (s/t)}'
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

# Time-to-first-frame for the "startup" scenario: poll the daemon's own log
# for NullTransport's one-time "first frame sent" marker (src/transport/null.rs)
# and diff its wall-clock arrival against the process's /proc start time.
# Writes ttff_ms=<value|n/a> to $out_dir/${pass}_ttff.txt. Bounded — a daemon
# that never renders a first frame must not hang the sweep.
measure_ttff() {
  local scenario="$1" pass="$2" pid="$3" log_file="$4" out_dir="$5"
  local spawn_epoch
  spawn_epoch=$(proc_start_epoch "$pid")
  local ttff_ms="n/a"

  if [[ -n "$spawn_epoch" ]]; then
    local waited=0 max_wait=100 # 100 * 0.05s = 5s
    while ! grep -q "NullTransport: first frame sent" "$log_file" 2>/dev/null; do
      if [[ "$waited" -ge "$max_wait" ]]; then
        echo "!! [$scenario/$pass] first-frame marker not seen within 5s — ttff_ms not recorded" >&2
        break
      fi
      sleep 0.05
      waited=$((waited + 1))
    done
    if grep -q "NullTransport: first frame sent" "$log_file" 2>/dev/null; then
      local first_frame_epoch
      first_frame_epoch=$(date +%s.%N)
      ttff_ms=$(awk -v s="$spawn_epoch" -v e="$first_frame_epoch" 'BEGIN{printf "%.1f", (e-s)*1000}')
    fi
  fi

  echo "ttff_ms=$ttff_ms" > "$out_dir/${pass}_ttff.txt"
}

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
  CURRENT_SCRATCH="$scratch"
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
  CURRENT_WRAPPER_PID="$wrapper_pid"

  # Anchored match: the dbus-run-session wrapper's own cmdline also contains
  # "$bin daemon" as a substring, so an unanchored pgrep would match both.
  # NOTE: this still assumes a single profile.sh instance targeting this
  # binary path — acquire_lock() in main() guards against concurrent runs.
  local pid=""
  for _ in $(seq 1 50); do
    pid=$(pgrep -f "^$bin daemon\$" 2>/dev/null | head -1 || true)
    [[ -n "$pid" ]] && break
    sleep 0.1
  done
  if [[ -z "$pid" ]]; then
    echo "!! [$scenario/$pass] daemon did not start within 5s (see $log_file)" >&2
    kill -TERM "$wrapper_pid" 2>/dev/null || true
    wait "$wrapper_pid" 2>/dev/null || true
    CURRENT_WRAPPER_PID=""
    rm -rf "$scratch"
    CURRENT_SCRATCH=""
    return 1
  fi
  CURRENT_DAEMON_PID="$pid"

  if [[ "$scenario" == "startup" ]]; then
    measure_ttff "$scenario" "$pass" "$pid" "$log_file" "$out_dir"
  fi

  if [[ "$warmup" -gt 0 ]]; then
    sleep "$warmup"
  fi

  # Captured AFTER warmup, not before: one-time startup costs (fontdb load,
  # config/layout seeding) belong to the warmup window the caller asked us to
  # exclude, not to the steady-state cpu_per_frame_ms this produces.
  local start_ticks
  start_ticks=$(proc_cpu_ticks "$pid")

  if [[ "$pass" == "cpu" ]]; then
    local rss_csv="$out_dir/rss_timeline.csv"
    echo "elapsed_seconds,rss_kb" > "$rss_csv"
    sample_rss "$pid" "$measure" "$rss_csv" &
    local rss_pid=$!
    CURRENT_RSS_PID="$rss_pid"

    local perf_data="$out_dir/perf.data"
    if [[ "$HAVE_PERF" == 1 ]]; then
      # --no-inherit: perf's default is to follow newly-forked children of
      # the attached PID, which would fold nvidia-smi poll children into the
      # daemon's own flamegraph — exactly what the plan's "daemon PID only"
      # decision excludes. Neither reviewer had perf on hand to confirm this
      # flag does what the docs say in practice — verify on the first real
      # `--hardware`/sweep run with perf installed.
      perf record --call-graph dwarf --no-inherit -p "$pid" -o "$perf_data" -- sleep "$measure" 2>>"$log_file" || true
    else
      sleep "$measure"
    fi
    wait "$rss_pid" 2>/dev/null || true
    CURRENT_RSS_PID=""
  else
    sleep "$measure"
  fi

  local end_ticks
  end_ticks=$(proc_cpu_ticks "$pid")

  # Positively confirm the daemon was alive right up to our own SIGTERM —
  # otherwise a mid-window crash (proc_cpu_ticks falling back to 0 on a gone
  # /proc/<pid>/stat) silently produces negative cpu_seconds and a
  # frames_sent=0 that's indistinguishable from "legitimately sent 0 frames".
  local daemon_ok=1
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "!! [$scenario/$pass] daemon (pid $pid) was already dead before SIGTERM — mid-window crash, metrics unreliable (see $log_file)" >&2
    daemon_ok=0
  fi

  kill -TERM "$pid" 2>/dev/null || true

  # Bounded wait for clean shutdown. Never SIGKILL — dhat only writes its
  # allocation report on `Profiler` drop, which requires a clean process
  # exit; SIGKILL-ing a hung daemon would silently lose that data. If it
  # doesn't exit in time, say so loudly, mark the scenario failed, and move
  # on rather than hanging the whole sweep.
  local waited=0 hung=0
  while kill -0 "$pid" 2>/dev/null; do
    if [[ "$waited" -ge "$SHUTDOWN_TIMEOUT_SECONDS" ]]; then
      echo "!! [$scenario/$pass] daemon (pid $pid) didn't exit after ${SHUTDOWN_TIMEOUT_SECONDS}s; not killing (dhat output would be lost) — investigate PID $pid" >&2
      daemon_ok=0
      hung=1
      break
    fi
    sleep 1
    waited=$((waited + 1))
  done
  CURRENT_DAEMON_PID=""

  if [[ "$hung" -eq 1 ]]; then
    # The wrapper (dbus-run-session) won't exit until its child (the hung
    # daemon) does — blocking on `wait` here would hang the whole sweep on
    # this one scenario. Leave both running, untracked, for investigation.
    CURRENT_WRAPPER_PID=""
  else
    wait "$wrapper_pid" 2>/dev/null || true
    CURRENT_WRAPPER_PID=""
  fi

  # Second half of the positive-confirmation check: the daemon's own SIGTERM
  # handler logs "thermalwriter shutdown complete" as its last line, right
  # after transport.close() (which logs "NullTransport closed: N frames
  # sent"). Its absence means the process went away some other way even if
  # the kill -0 loop above saw it exit (e.g. it was reaped by something else).
  if [[ "$daemon_ok" -eq 1 ]] && ! grep -q "thermalwriter shutdown complete" "$log_file"; then
    echo "!! [$scenario/$pass] daemon exited but no clean-shutdown log line found (see $log_file)" >&2
    daemon_ok=0
  fi

  local frames_sent
  frames_sent=$(grep -o 'NullTransport closed: [0-9]* frames sent' "$log_file" | grep -o '[0-9]*' | head -1 || true)
  frames_sent="${frames_sent:-0}"

  if [[ "$pass" == "cpu" ]]; then
    if [[ "$daemon_ok" -ne 1 ]]; then
      {
        echo "status=ERROR"
        echo "error=daemon did not exit cleanly during the measurement window (see ${pass}_daemon.log)"
      } > "$out_dir/cpu_metrics.txt"
    else
      local tck cpu_seconds cpu_per_frame_ms
      tck=$(clk_tck)
      cpu_seconds=$(awk -v s="$start_ticks" -v e="$end_ticks" -v t="$tck" 'BEGIN{printf "%.4f", (e-s)/t}')
      if [[ "$frames_sent" -gt 0 ]]; then
        cpu_per_frame_ms=$(awk -v c="$cpu_seconds" -v f="$frames_sent" 'BEGIN{printf "%.3f", (c*1000)/f}')
      else
        cpu_per_frame_ms="n/a"
      fi
      {
        echo "status=OK"
        echo "cpu_seconds=$cpu_seconds"
        echo "frames_sent=$frames_sent"
        echo "cpu_per_frame_ms=$cpu_per_frame_ms"
      } > "$out_dir/cpu_metrics.txt"
    fi

    if [[ "$HAVE_PERF" == 1 && "$HAVE_INFERNO" == 1 && -f "$perf_data" ]]; then
      # Under `set -o pipefail`, a nonzero exit from ANY stage fails this
      # pipeline — but on the xvfb-conky scenario this was observed to fire
      # even when flamegraph.svg came out fine, most likely `perf script`/
      # `perf record` grumbling about a followed child (Xvfb/conky) that
      # exited during teardown while being traced. --no-inherit above should
      # eliminate that (we no longer follow forked children at all), but
      # judge success by the artifact actually existing rather than trusting
      # the pipeline's exit code, in case some other transient still trips it.
      perf script -i "$perf_data" 2>>"$log_file" | inferno-collapse-perf 2>>"$log_file" | inferno-flamegraph > "$out_dir/flamegraph.svg" 2>>"$log_file" || true
      if [[ ! -s "$out_dir/flamegraph.svg" ]]; then
        echo "!! [$scenario/cpu] flamegraph generation failed (see $log_file)" >&2
      fi
    fi
  else
    if [[ -f "$scratch/dhat-heap.json" ]]; then
      mv "$scratch/dhat-heap.json" "$out_dir/dhat-heap.json"
    fi
    if [[ "$daemon_ok" -ne 1 ]]; then
      {
        echo "status=ERROR"
        echo "error=daemon did not exit cleanly during the measurement window (see ${pass}_daemon.log)"
      } > "$out_dir/dhat_metrics.txt"
    else
      # dhat-heap.json has no flat "total bytes" field (it's a per-callsite
      # table under "pps") — the aggregate totals are on dhat's own stderr
      # summary, which we've already captured in $log_file since it's written
      # on Profiler drop, right after our SIGTERM.
      {
        echo "status=OK"
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
  fi

  rm -rf "$scratch"
  CURRENT_SCRATCH=""

  if [[ "$daemon_ok" -ne 1 ]]; then
    return 1
  fi
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
    echo "| scenario | cpu/frame (ms) | frames sent | avg RSS (KB) | peak RSS (KB) | total allocated (bytes) | time to first frame (ms) |"
    echo "|---|---|---|---|---|---|---|"
    for s in "${scenarios[@]}"; do
      local out_dir="$RESULTS_DIR/$s"
      local cpu_per_frame="n/a" frames="n/a" avg_rss="n/a" peak_rss="n/a" total_bytes="n/a" ttff="n/a"

      local cpu_status=""
      [[ -f "$out_dir/cpu_metrics.txt" ]] && cpu_status=$(awk -F= '/^status=/{print $2}' "$out_dir/cpu_metrics.txt")
      if [[ "$cpu_status" == "ERROR" ]]; then
        cpu_per_frame="ERROR"
        frames="ERROR"
      elif [[ -f "$out_dir/cpu_metrics.txt" ]]; then
        cpu_per_frame=$(awk -F= '/cpu_per_frame_ms/{print $2}' "$out_dir/cpu_metrics.txt")
        frames=$(awk -F= '/frames_sent/{print $2}' "$out_dir/cpu_metrics.txt")
      fi

      if [[ "$cpu_status" != "ERROR" && -f "$out_dir/rss_timeline.csv" ]]; then
        avg_rss=$(awk -F, 'NR>1{s+=$2;n++} END{if(n>0) printf "%.0f", s/n; else print "n/a"}' "$out_dir/rss_timeline.csv")
        peak_rss=$(awk -F, 'NR>1{if($2>m)m=$2} END{print (m>0)?m:"n/a"}' "$out_dir/rss_timeline.csv")
      fi

      local dhat_status=""
      [[ -f "$out_dir/dhat_metrics.txt" ]] && dhat_status=$(awk -F= '/^status=/{print $2}' "$out_dir/dhat_metrics.txt")
      if [[ "$dhat_status" == "ERROR" ]]; then
        total_bytes="ERROR"
      elif [[ -f "$out_dir/dhat_metrics.txt" ]]; then
        total_bytes=$(awk -F= '/total_bytes_allocated/{print $2}' "$out_dir/dhat_metrics.txt")
      fi

      # Only the "startup" scenario writes this — time-to-first-frame is
      # meaningless for the rest, where the daemon has been rendering for a
      # full warmup window before we ever start measuring.
      [[ -f "$out_dir/cpu_ttff.txt" ]] && ttff=$(awk -F= '/^ttff_ms=/{print $2}' "$out_dir/cpu_ttff.txt")

      echo "| $s | $cpu_per_frame | $frames | $avg_rss | $peak_rss | $total_bytes | $ttff |"
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

  acquire_lock
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
    # A crashed/hung daemon makes run_pass return 1 (after already writing an
    # explicit status=ERROR marker for this scenario) — one bad scenario
    # must not abort the rest of the sweep under `set -e`.
    run_pass "$s" "$PROFILE_BIN" "cpu" || true
  done

  echo "== Building profiling profile + dhat-heap (allocation pass) =="
  cargo build --profile profiling --features dhat-heap

  for s in "${run_list[@]}"; do
    run_pass "$s" "$PROFILE_BIN" "dhat" || true
  done

  emit_summary "${run_list[@]}"
}

main "$@"
