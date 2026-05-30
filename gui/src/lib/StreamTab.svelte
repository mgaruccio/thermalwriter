<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { configDir } from "@tauri-apps/api/path";
  import { open as dialogOpen } from "@tauri-apps/plugin-dialog";
  import {
    STREAM_PRESETS,
    TERMINAL_BINARIES,
    allBinariesToResolve,
    buildArgv,
  } from "./streamPresets";

  // Prefix of AppError::NoFrame's serialized Display string.
  // AppError serializes to a plain string (not a structured object), so we
  // match on the canonical prefix from the thiserror #[error("...")] template.
  const NO_FRAME_PREFIX = "no stream frame available:";

  // Props
  type Props = {
    /** Currently selected layout name — used as the return-to target on Stop. */
    selectedLayout: string;
    /** Callback to notify App.svelte that the daemon state may have changed. */
    onDaemonStateChange?: () => void;
    /**
     * Whether the Stream tab is currently visible to the user.
     * Poll ticks are skipped (but the interval keeps running) when false,
     * saving decode + draw work on a hidden panel without pausing the stream.
     */
    tabVisible?: boolean;
  };
  let { selectedLayout, onDaemonStateChange, tabVisible = true }: Props = $props();

  // ── State ──────────────────────────────────────────────────────────────────

  let selectedPresetId = $state(STREAM_PRESETS[0].id);
  let fps = $state(STREAM_PRESETS[0].default_fps);
  let streaming = $state(false);
  let starting = $state(false);
  let stopping = $state(false);
  let status = $state("");
  let error = $state("");

  // Live preview: current Blob URL for the <img> element.
  // Revoked before each new frame and in onDestroy to avoid memory leaks.
  let frameBlobUrl = $state<string | null>(null);
  let pollInterval: ReturnType<typeof setInterval> | undefined;
  // Guard: prevents stacking invoke calls when a tick takes > 333ms.
  let pollInFlight = false;

  // Resolved binary paths — subset of allBinariesToResolve() keys that are present.
  // Absent key = binary not found on system.
  let resolved = $state<Record<string, string>>({});
  let resolving = $state(true);

  // Per-preset field values keyed by field.kind.
  let fieldValues = $state<Record<string, string>>({});
  // Wrapper config dir base path for defaults (set once in onMount).
  let wrapperDir = $state("~/.config/thermalwriter/wrappers");

  // ── Derived ───────────────────────────────────────────────────────────────

  const preset = $derived(
    STREAM_PRESETS.find((p) => p.id === selectedPresetId) ?? STREAM_PRESETS[0],
  );

  // Xvfb must be resolvable for streaming to work at all.
  const xvfbResolved = $derived(!!resolved["Xvfb"]);

  // The selected preset's binary is resolved (or it's the custom preset).
  const presetBinaryResolved = $derived(
    preset.binary === "" || !!resolved[preset.binary],
  );

  // For terminal-wrapped presets: at least one terminal emulator is available.
  const terminalResolved = $derived(
    !preset.needs_terminal ||
      TERMINAL_BINARIES.some((t) => !!resolved[t]),
  );

  // Custom preset requires a non-empty path before Start is enabled.
  const customPathReady = $derived(
    preset.id !== "custom" || !!fieldValues["custom_path"],
  );

  // Start is available when everything required is resolved.
  const canStart = $derived(
    !streaming &&
      !starting &&
      xvfbResolved &&
      presetBinaryResolved &&
      terminalResolved &&
      customPathReady,
  );

  // Hint explaining why Start is greyed out.
  const unavailableHint = $derived(
    resolving
      ? "Checking installed tools…"
      : !xvfbResolved
        ? "Xvfb not found — install xorg-server"
        : !presetBinaryResolved
          ? `${preset.binary} not found — install it to use this preset`
          : !terminalResolved
            ? "No terminal emulator found (alacritty / kitty / xterm)"
            : !customPathReady
              ? "Enter or browse for the executable path"
              : "",
  );

  // Resolved terminal path (first found, in preference order).
  const resolvedTerminal = $derived(
    TERMINAL_BINARIES.map((t) => resolved[t] ?? null).find(Boolean) ?? null,
  );

  // ── Lifecycle ──────────────────────────────────────────────────────────────

  onMount(async () => {
    // Resolve the wrapper config directory.
    try {
      const cfg = await configDir();
      wrapperDir = `${cfg}/thermalwriter/wrappers`;
    } catch {
      // configDir() unavailable (e.g. browser dev mode) — keep the tilde fallback.
    }
    fieldValues = {
      config_path: `${wrapperDir}/${preset.id}-480.conf`,
      custom_path: "",
    };

    // Probe the daemon for its current mode. If mode === "xvfb", a stream was
    // started before this GUI opened — auto-attach to it.
    try {
      const s = await invoke<{ mode: string; tick_rate: number }>("get_status");
      if (s.mode === "xvfb") {
        streaming = true;
        fps = s.tick_rate || fps;
        status = "Stream already running — attached to live feed.";
        startPoll();
      }
    } catch {
      // Daemon offline or get_status unavailable — not streaming.
    }

    await doResolve();
  });

  onDestroy(() => {
    stopPoll();
    revokeBlobUrl();
  });

  // When the preset changes, update fps default and reset config_path hint.
  $effect(() => {
    fps = preset.default_fps;
    const suffix =
      preset.id === "conky" || preset.id === "cava"
        ? `${preset.id}-480.conf`
        : "";
    if (suffix) {
      fieldValues = {
        ...fieldValues,
        config_path: `${wrapperDir}/${suffix}`,
      };
    }
  });

  // ── Poll loop ─────────────────────────────────────────────────────────────

  function startPoll() {
    if (pollInterval !== undefined) return; // already running
    // ~3 FPS for the GUI preview (independent of the LCD tick rate).
    pollInterval = setInterval(pollFrame, 333);
  }

  function stopPoll() {
    if (pollInterval !== undefined) {
      clearInterval(pollInterval);
      pollInterval = undefined;
    }
    pollInFlight = false;
  }

  async function pollFrame() {
    if (pollInFlight) return; // don't stack if a tick takes >333ms
    // Skip decode + draw when the tab is hidden — saves CPU on a 1%-CPU device.
    // The interval keeps ticking so the stream resumes instantly on tab-show.
    if (!tabVisible) return;
    pollInFlight = true;
    try {
      const bytes = await invoke<ArrayBuffer>("read_frame");
      paintFrame(bytes);
    } catch (e) {
      const msg = String(e);
      if (msg.includes(NO_FRAME_PREFIX)) {
        // Frame cleared — stream ended externally; sync UI state.
        streaming = false;
        status = "Stream ended externally.";
        stopPoll();
        revokeBlobUrl();
      }
      // Other errors (transient IPC) — silently skip this tick.
    } finally {
      pollInFlight = false;
    }
  }

  // ── Rendering ─────────────────────────────────────────────────────────────

  /**
   * Update the live preview img with fresh JPEG bytes.
   *
   * The daemon writes frames post-rotation (180° for the physical LCD mount).
   * The <img> carries CSS `transform: rotate(180deg)` to display right-side-up.
   * The previous Blob URL is revoked before creating a new one to prevent leaks.
   */
  function paintFrame(bytes: ArrayBuffer) {
    revokeBlobUrl();
    const blob = new Blob([bytes], { type: "image/jpeg" });
    frameBlobUrl = URL.createObjectURL(blob);
  }

  function revokeBlobUrl() {
    if (frameBlobUrl) {
      URL.revokeObjectURL(frameBlobUrl);
      frameBlobUrl = null;
    }
  }

  // ── Helpers ────────────────────────────────────────────────────────────────

  async function doResolve() {
    resolving = true;
    error = "";
    try {
      const names = [...allBinariesToResolve(), "Xvfb"];
      resolved = await invoke<Record<string, string>>("resolve_binaries", { names });
    } catch (e) {
      // Daemon offline — treat all as unresolved; show a clear message.
      resolved = {};
      error = `Could not reach daemon to resolve binaries: ${e}`;
    } finally {
      resolving = false;
    }
  }

  async function startStream() {
    error = "";
    status = "";
    starting = true;
    try {
      const argv = buildArgv(preset, resolved, fieldValues, resolvedTerminal);
      if (!argv) {
        error = "Could not build launch command — check that required binaries are installed.";
        return;
      }
      await invoke<void>("apply_stream", { argv });
      // Push the desired tick rate. The daemon auto-restores the prior rate on
      // stop_stream, so we don't need to track or restore it here.
      try {
        await invoke<void>("set_tick_rate", { rate: fps });
      } catch (rateErr) {
        // Non-fatal: stream is running but may not be at the requested FPS.
        // Surface as a warning rather than rolling back.
        status = `Streaming ${preset.label} (FPS set failed: ${rateErr})`;
      }
      streaming = true;
      if (!status) status = `Streaming ${preset.label} at ${fps} FPS`;
      onDaemonStateChange?.();
      startPoll();
    } catch (e) {
      error = `Start failed: ${e}`;
    } finally {
      starting = false;
    }
  }

  async function stopStream() {
    error = "";
    stopping = true;
    try {
      await invoke<void>("stop_stream", { layout: selectedLayout });
      streaming = false;
      status = `Stopped. Returned to ${selectedLayout}.`;
      onDaemonStateChange?.();
      stopPoll();
      revokeBlobUrl();
    } catch (e) {
      error = `Stop failed: ${e}`;
    } finally {
      stopping = false;
    }
  }

  async function browseForCustomPath() {
    try {
      const selected = await dialogOpen({
        title: "Select executable",
        multiple: false,
        directory: false,
      });
      // dialogOpen returns string | string[] | null
      if (typeof selected === "string" && selected) {
        fieldValues = { ...fieldValues, custom_path: selected };
      }
    } catch {
      // User cancelled or dialog unavailable — leave field unchanged.
    }
  }

  function resolvedClass(binary: string): string {
    if (resolving) return "status-probing";
    return resolved[binary] ? "status-ok" : "status-missing";
  }
</script>

<div class="stream-tab">
  <!-- ── Live preview (only while streaming) ── -->
  {#if streaming}
    <div class="preview-wrap">
      {#if frameBlobUrl}
        <!--
          The daemon writes frames post-rotation (180° for the physical LCD mount).
          CSS transform: rotate(180deg) un-rotates for right-side-up display here.
          Blob URL is revoked on each new frame and in onDestroy (no leaks).
        -->
        <img
          src={frameBlobUrl}
          alt="Live stream preview"
          class="stream-preview"
          style="transform: rotate(180deg)"
        />
      {:else}
        <div class="preview-placeholder">
          <span class="preview-wait">Waiting for first frame…</span>
        </div>
      {/if}
      <div class="preview-badge">LIVE</div>
    </div>
  {/if}

  <!-- ── Xvfb gate banner ── -->
  {#if !resolving && !xvfbResolved}
    <div class="gate-banner">
      <span class="gate-icon">⚠</span>
      <span>
        <strong>Xvfb not found.</strong>
        Install <code>xorg-server</code> (or <code>xorg-xvfb</code>) to enable streaming.
      </span>
    </div>
  {/if}

  <!-- ── Preset selector ── -->
  <div class="stream-row">
    <label class="field-label" for="preset-select">App</label>
    <div class="preset-selector">
      {#each STREAM_PRESETS as p}
        <button
          type="button"
          class="preset-btn"
          class:active={selectedPresetId === p.id}
          class:unavailable={p.binary !== "" && !resolving && !resolved[p.binary]}
          disabled={streaming}
          onclick={() => { selectedPresetId = p.id; }}
          title={p.binary !== "" && !resolved[p.binary] && !resolving
            ? `${p.binary} not installed`
            : p.label}
        >
          <span class="preset-dot {resolvedClass(p.binary)}"></span>
          <span>{p.label}</span>
        </button>
      {/each}
    </div>
  </div>

  <!-- ── Per-preset fields ── -->
  {#if preset.fields.length > 0}
    <div class="stream-fields">
      {#each preset.fields as field}
        {#if field.kind === "config_path"}
          <div class="stream-row">
            <label class="field-label" for="config-path-input">{field.label}</label>
            <input
              id="config-path-input"
              type="text"
              value={fieldValues["config_path"] ?? ""}
              oninput={(e) => { fieldValues = { ...fieldValues, config_path: e.currentTarget.value }; }}
              disabled={streaming}
              placeholder="~/.config/thermalwriter/wrappers/…"
            />
          </div>
        {:else if field.kind === "custom_path"}
          <div class="stream-row">
            <label class="field-label" for="custom-path-input">{field.label}</label>
            <div class="custom-path-row">
              <input
                id="custom-path-input"
                type="text"
                value={fieldValues["custom_path"] ?? ""}
                oninput={(e) => { fieldValues = { ...fieldValues, custom_path: e.currentTarget.value }; }}
                disabled={streaming}
                placeholder="/usr/bin/my-app"
              />
              <button
                type="button"
                class="btn-browse"
                disabled={streaming}
                onclick={browseForCustomPath}
                title="Browse for executable"
              >Browse</button>
            </div>
          </div>
        {/if}
      {/each}
    </div>
  {/if}

  <!-- ── Terminal emulator status (terminal-wrapped presets) ── -->
  {#if preset.needs_terminal}
    <div class="stream-row terminal-row">
      <span class="field-label">Terminal</span>
      <div class="terminal-list">
        {#each TERMINAL_BINARIES as t}
          <span class="terminal-chip {resolvedClass(t)}" title={resolved[t] ?? "not found"}>
            <span class="terminal-dot"></span>
            {t}
          </span>
        {/each}
      </div>
    </div>
  {/if}

  <!-- ── FPS control ── -->
  <div class="stream-row">
    <label class="field-label" for="fps-input">FPS</label>
    <div class="fps-control">
      <input
        type="range"
        min="1"
        max="60"
        step="1"
        value={fps}
        oninput={(e) => { fps = Number(e.currentTarget.value); }}
        disabled={streaming}
      />
      <input
        id="fps-input"
        type="number"
        class="fps-number"
        min="1"
        max="60"
        value={fps}
        oninput={(e) => { fps = Math.max(1, Math.min(60, Number(e.currentTarget.value))); }}
        disabled={streaming}
      />
      <span class="fps-unit">fps</span>
    </div>
  </div>

  <!-- ── Start / Stop ── -->
  <div class="stream-actions">
    {#if !streaming}
      <button
        type="button"
        class="btn-start"
        onclick={startStream}
        disabled={!canStart || starting}
        title={unavailableHint || undefined}
      >
        {starting ? "Starting…" : "▶ Start stream"}
      </button>
    {:else}
      <button
        type="button"
        class="btn-stop"
        onclick={stopStream}
        disabled={stopping}
      >
        {stopping ? "Stopping…" : "■ Stop stream"}
      </button>
    {/if}

    <button
      type="button"
      class="btn-refresh"
      onclick={doResolve}
      disabled={resolving || streaming}
      title="Re-check installed tools"
    >
      {resolving ? "⧖" : "↻"}
    </button>
  </div>

  <!-- ── Unavailable hint ── -->
  {#if unavailableHint && !streaming}
    <p class="hint">{unavailableHint}</p>
  {/if}

  <!-- ── Status / Error ── -->
  {#if status}
    <p class="stream-status">{status}</p>
  {/if}
  {#if error}
    <p class="stream-error">{error}</p>
  {/if}

  <!-- ── Binary resolution table ── -->
  <div class="resolve-table">
    <div class="section-label"><span>Detected tools</span></div>
    {#each STREAM_PRESETS.filter((p) => p.binary !== "") as p}
      <div class="resolve-row">
        <span class="resolve-dot {resolvedClass(p.binary)}"></span>
        <span class="resolve-name">{p.binary}</span>
        <span class="resolve-path">
          {resolving
            ? "probing…"
            : resolved[p.binary]
              ? resolved[p.binary]
              : "not found"}
        </span>
      </div>
    {/each}
    <div class="resolve-row">
      <span class="resolve-dot {resolvedClass('Xvfb')}"></span>
      <span class="resolve-name">Xvfb</span>
      <span class="resolve-path">
        {resolving ? "probing…" : resolved["Xvfb"] ?? "not found"}
      </span>
    </div>
  </div>
</div>

<style>
  .stream-tab {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 4px 0;
  }

  /* ── Live preview ── */
  .preview-wrap {
    position: relative;
    display: flex;
    justify-content: center;
  }

  /* Scale down from 480px source to fit the ~320px-wide config pane. */
  .stream-preview {
    width: 100%;
    max-width: 280px;
    height: auto;
    aspect-ratio: 1 / 1;
    border-radius: 8px;
    background: #050608;
    border: 1px solid var(--line-strong);
    box-shadow:
      inset 0 0 0 1px color-mix(in srgb, var(--amber) 30%, transparent),
      0 0 24px -8px color-mix(in srgb, var(--amber) 45%, transparent);
    display: block;
    object-fit: cover;
  }

  .preview-placeholder {
    width: 100%;
    max-width: 280px;
    height: 80px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--bg-deep) 60%, transparent);
    border: 1px dashed var(--line-strong);
    border-radius: var(--radius-md);
  }

  .preview-wait {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.14em;
    color: var(--text-dim);
    animation: pulse-text 1.6s ease-in-out infinite;
  }

  @keyframes pulse-text {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.4; }
  }

  .preview-badge {
    position: absolute;
    top: 6px;
    right: calc(50% - 140px + 6px);
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.3em;
    padding: 2px 6px;
    background: color-mix(in srgb, var(--amber) 90%, transparent);
    color: var(--bg-deep);
    border-radius: 3px;
    text-transform: uppercase;
    animation: live-pulse 2s ease-in-out infinite;
  }

  @keyframes live-pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.6; }
  }

  /* ── Gate banner ── */
  .gate-banner {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 14px;
    background: color-mix(in srgb, var(--red) 14%, var(--bg-elev));
    border: 1px solid color-mix(in srgb, var(--red) 40%, transparent);
    border-radius: var(--radius-md);
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text-primary);
  }
  .gate-icon {
    font-size: 15px;
    color: var(--red);
    flex-shrink: 0;
  }
  .gate-banner code {
    font-family: var(--font-mono);
    color: var(--cyan);
  }

  /* ── Rows ── */
  .stream-row {
    display: grid;
    grid-template-columns: 72px 1fr;
    align-items: center;
    gap: 10px;
  }

  .field-label {
    font-family: var(--font-mono);
    font-size: 10.5px;
    letter-spacing: 0.2em;
    text-transform: uppercase;
    color: var(--text-dim);
    white-space: nowrap;
  }

  /* ── Preset selector ── */
  .preset-selector {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .preset-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 5px 10px;
    font-size: 11.5px;
    text-transform: none;
    letter-spacing: 0.02em;
  }

  .preset-btn.active {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent-soft) 70%, var(--bg-elev));
    color: var(--text-primary);
    box-shadow: 0 0 0 1px var(--accent-soft);
  }

  .preset-btn.unavailable {
    opacity: 0.5;
  }

  /* ── Status dots ── */
  .preset-dot,
  .resolve-dot,
  .terminal-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .status-ok {
    background: var(--green);
    box-shadow: 0 0 5px color-mix(in srgb, var(--green) 60%, transparent);
  }

  .status-missing {
    background: var(--red);
    box-shadow: 0 0 5px color-mix(in srgb, var(--red) 50%, transparent);
  }

  .status-probing {
    background: var(--amber);
    box-shadow: 0 0 5px color-mix(in srgb, var(--amber) 50%, transparent);
    animation: pulse-led 1.4s ease-in-out infinite;
  }

  @keyframes pulse-led {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.4; }
  }

  /* ── Fields ── */
  .stream-fields {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .custom-path-row {
    display: flex;
    gap: 8px;
    align-items: center;
  }

  .custom-path-row input {
    flex: 1;
  }

  .btn-browse {
    flex-shrink: 0;
    padding: 0.5rem 0.7rem;
    font-size: 11px;
  }

  /* ── Terminal chips ── */
  .terminal-row {
    align-items: flex-start;
    padding-top: 2px;
  }

  .terminal-list {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .terminal-chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 3px 8px;
    background: color-mix(in srgb, var(--bg-deep) 70%, transparent);
    border: 1px solid var(--line-soft);
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-muted);
  }

  /* ── FPS control ── */
  .fps-control {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .fps-control input[type="range"] {
    flex: 1;
    width: auto;
    padding: 0;
    border: none;
    background: transparent;
    accent-color: var(--accent);
  }

  .fps-number {
    width: 54px !important;
    padding: 0.45rem 0.5rem;
    text-align: center;
  }

  .fps-unit {
    font-family: var(--font-mono);
    font-size: 10.5px;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--text-dim);
  }

  /* ── Actions ── */
  .stream-actions {
    display: flex;
    gap: 8px;
    align-items: center;
  }

  .btn-start {
    flex: 1;
    justify-content: center;
    background: color-mix(in srgb, var(--green) 18%, var(--bg-elev));
    border-color: color-mix(in srgb, var(--green) 45%, transparent);
    color: var(--green);
  }

  .btn-start:hover:not(:disabled) {
    background: color-mix(in srgb, var(--green) 28%, var(--bg-elev));
    border-color: var(--green);
    box-shadow: 0 0 12px color-mix(in srgb, var(--green) 30%, transparent);
  }

  .btn-stop {
    flex: 1;
    justify-content: center;
    background: color-mix(in srgb, var(--red) 18%, var(--bg-elev));
    border-color: color-mix(in srgb, var(--red) 45%, transparent);
    color: var(--red);
    animation: stream-pulse 2.4s ease-in-out infinite;
  }

  .btn-stop:hover:not(:disabled) {
    background: color-mix(in srgb, var(--red) 28%, var(--bg-elev));
    border-color: var(--red);
    box-shadow: 0 0 12px color-mix(in srgb, var(--red) 30%, transparent);
    animation: none;
  }

  @keyframes stream-pulse {
    0%, 100% { border-color: color-mix(in srgb, var(--red) 45%, transparent); }
    50% { border-color: color-mix(in srgb, var(--red) 80%, transparent); }
  }

  .btn-refresh {
    flex-shrink: 0;
    width: 36px;
    padding: 0.5rem 0;
    justify-content: center;
    font-size: 15px;
    letter-spacing: 0;
    text-transform: none;
  }

  /* ── Hint / status / error ── */
  .hint {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--amber);
    padding: 6px 10px;
    background: color-mix(in srgb, var(--amber) 10%, var(--bg-elev));
    border: 1px solid color-mix(in srgb, var(--amber) 30%, transparent);
    border-radius: var(--radius-sm);
  }

  .stream-status {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--green);
    padding: 6px 10px;
    background: color-mix(in srgb, var(--green) 10%, var(--bg-elev));
    border: 1px solid color-mix(in srgb, var(--green) 25%, transparent);
    border-radius: var(--radius-sm);
  }

  .stream-error {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--red);
    padding: 6px 10px;
    background: color-mix(in srgb, var(--red) 10%, var(--bg-elev));
    border: 1px solid color-mix(in srgb, var(--red) 25%, transparent);
    border-radius: var(--radius-sm);
  }

  /* ── Resolution table ── */
  .resolve-table {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: 4px;
  }

  .resolve-row {
    display: grid;
    grid-template-columns: 12px 80px 1fr;
    align-items: center;
    gap: 8px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-muted);
    padding: 3px 4px;
    border-radius: var(--radius-sm);
  }

  .resolve-row:hover {
    background: color-mix(in srgb, var(--bg-hover) 50%, transparent);
  }

  .resolve-name {
    color: var(--text-primary);
    font-size: 11.5px;
  }

  .resolve-path {
    color: var(--text-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 10.5px;
  }

  /* ── section-label reuse ── */
  .section-label {
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.28em;
    text-transform: uppercase;
    color: var(--text-dim);
    padding: 10px 4px 4px;
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .section-label::after {
    content: "";
    flex: 1;
    height: 1px;
    background: linear-gradient(90deg, var(--line-strong), transparent);
  }
</style>
