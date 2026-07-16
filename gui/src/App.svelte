<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import BgGallery from "./lib/BgGallery.svelte";
  import StreamTab from "./lib/StreamTab.svelte";

  // Canonical prefix of AppError::DaemonUnavailable's serialized Display string.
  // AppError serializes to a plain string; this prefix is guaranteed by the
  // thiserror #[error("daemon is not running…")] template in error.rs.
  const DAEMON_OFFLINE_PREFIX = "daemon is not running";
  const DAEMON_STATUS_POLL_MS = 5000;

  type LayoutSummary = {
    name: string;
    kind: string;
    configurable: boolean;
  };

  type VariableDecl = {
    name: string;
    type: "color" | "text" | "sensor" | "number";
    default: string;
    help: string;
    value: string;
    min: number | null;
    max: number | null;
    step: number | null;
  };

  type SensorDescriptor = {
    key: string;
    name: string;
    unit: string;
  };

  type DaemonStatus = {
    mode: string;
    tick_rate: number;
    connected: boolean;
    active_layout: string;
    resolution: string;
  };

  type ThemeId =
    | "tokyo-night-storm"
    | "tokyo-night"
    | "catppuccin-mocha"
    | "gruvbox-material"
    | "nord";

  const THEMES: { id: ThemeId; label: string }[] = [
    { id: "tokyo-night-storm", label: "Tokyo Storm" },
    { id: "tokyo-night", label: "Tokyo Night" },
    { id: "catppuccin-mocha", label: "Catppuccin" },
    { id: "gruvbox-material", label: "Gruvbox" },
    { id: "nord", label: "Nord" },
  ];

  let layouts = $state<LayoutSummary[]>([]);
  let variables = $state<VariableDecl[]>([]);
  let sensors = $state<SensorDescriptor[]>([]);
  let values = $state<Record<string, string>>({});
  let selectedLayout = $state("");
  let backgrounds = $state<string[]>([]);
  let selectedBackground = $state<string | null>(null);
  let loading = $state(true);
  let previewing = $state(false);
  let applying = $state(false);
  let suggesting = $state(false);
  let status = $state("");
  let error = $state("");
  let canvas = $state<HTMLCanvasElement | undefined>();
  let previewTimer: number | undefined;
  let daemonState = $state<"unknown" | "ok" | "down">("unknown");
  let daemonStatus = $state<DaemonStatus | null>(null);
  let daemonProbeTimer: ReturnType<typeof setInterval> | undefined;
  let daemonProbeInFlight = false;
  let daemonProbeQueued = false;
  let appMounted = false;
  let activeTab = $state<"variables" | "stream">("variables");
  let theme = $state<ThemeId>(
    (localStorage.getItem("tw-theme") as ThemeId) || "tokyo-night-storm",
  );

  const selected = $derived(layouts.find((layout) => layout.name === selectedLayout));
  const hasColorVars = $derived(variables.some((variable) => variable.type === "color"));
  const configurableLayouts = $derived(layouts.filter((l) => l.configurable));
  const previewOnlyLayouts = $derived(layouts.filter((l) => !l.configurable));

  const activeDaemonLayout = $derived(daemonStatus?.active_layout ?? "");
  const titlebarResolution = $derived((daemonStatus?.resolution || "480x480").replace("x", " × "));
  const deviceConnected = $derived(daemonState === "ok" && daemonStatus?.connected === true);
  const deviceBadgeClass = $derived(daemonState === "down" ? "err" : deviceConnected ? "ok" : "warn");
  const deviceBadgeLabel = $derived(
    daemonState === "unknown"
      ? "Probing device"
      : daemonState === "down"
        ? "Daemon offline"
        : deviceConnected
          ? "USB connected"
          : "USB disconnected",
  );
  $effect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem("tw-theme", theme);
  });

  onMount(async () => {
    appMounted = true;
    // D-Bus status probing must not depend on startup metadata invokes. If a
    // layout/background read fails, keep polling so the titlebar can recover.
    void probeDaemon();
    if (appMounted && daemonProbeTimer === undefined) {
      daemonProbeTimer = setInterval(probeDaemon, DAEMON_STATUS_POLL_MS);
    }

    try {
      const [layoutList, sensorList, bgList, activeBg] = await Promise.all([
        invoke<LayoutSummary[]>("list_layouts"),
        invoke<SensorDescriptor[]>("list_sensors"),
        invoke<string[]>("list_backgrounds"),
        invoke<string | null>("get_active_background"),
      ]);
      layouts = layoutList;
      sensors = sensorList;
      backgrounds = bgList;
      selectedBackground = activeBg;
      const firstConfigurable = layouts.find((layout) => layout.configurable) ?? layouts[0];
      if (firstConfigurable) {
        await selectLayout(firstConfigurable.name);
      }
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

  onDestroy(() => {
    appMounted = false;
    daemonProbeQueued = false;
    if (daemonProbeTimer !== undefined) {
      clearInterval(daemonProbeTimer);
      daemonProbeTimer = undefined;
    }
  });

  async function probeDaemon() {
    if (daemonProbeInFlight) {
      daemonProbeQueued = true;
      return;
    }
    daemonProbeInFlight = true;
    try {
      const nextStatus = await invoke<DaemonStatus>("get_status");
      if (appMounted) {
        daemonStatus = nextStatus;
        daemonState = "ok";
      }
    } catch {
      if (appMounted) {
        daemonStatus = null;
        daemonState = "down";
      }
    } finally {
      daemonProbeInFlight = false;
      if (daemonProbeQueued && appMounted) {
        daemonProbeQueued = false;
        void probeDaemon();
      }
    }
  }

  $effect(() => {
    selectedLayout;
    selectedBackground;
    JSON.stringify(values);
    schedulePreview();
  });

  async function selectLayout(name: string) {
    selectedLayout = name;
    status = "";
    error = "";
    variables = await invoke<VariableDecl[]>("get_layout_vars", { layout: name });
    values = Object.fromEntries(variables.map((variable) => [variable.name, variable.value]));
    schedulePreview();
  }

  function setValue(name: string, value: string) {
    values = { ...values, [name]: value };
  }

  // Re-read the gallery after an import and select the freshly added file.
  async function refreshBackgrounds(selectName?: string) {
    try {
      backgrounds = await invoke<string[]>("list_backgrounds");
      if (selectName) selectedBackground = selectName;
    } catch (e) {
      error = String(e);
    }
  }

  function schedulePreview() {
    if (!selectedLayout || !canvas) return;
    if (previewTimer) window.clearTimeout(previewTimer);
    previewTimer = window.setTimeout(renderPreview, 120);
  }

  async function renderPreview() {
    if (!selectedLayout || !canvas) return;
    previewing = true;
    error = "";
    try {
      const buffer = await invoke<ArrayBuffer>("render_preview", {
        layout: selectedLayout,
        vars: values,
        background: selectedBackground,
      });
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("Canvas context unavailable");
      const image = new ImageData(new Uint8ClampedArray(buffer), 480, 480);
      ctx.putImageData(image, 0, 0);
    } catch (e) {
      error = String(e);
    } finally {
      previewing = false;
    }
  }

  async function apply() {
    if (!selectedLayout) return;
    applying = true;
    status = "";
    error = "";
    try {
      try {
        await invoke<void>("apply_to_daemon", {
          layout: selectedLayout,
          vars: values,
        });
        await invoke<void>("set_background", { name: selectedBackground });
        status = `Applied ${selectedLayout} — live on device.`;
        daemonState = "ok";
        await probeDaemon();
      } catch (e) {
        const message = String(e);
        if (message.includes(DAEMON_OFFLINE_PREFIX)) {
          await invoke<void>("save_config", {
            layout: selectedLayout,
            vars: values,
          });
          await invoke<void>("save_background", { name: selectedBackground });
          status = `Saved ${selectedLayout}. Daemon offline — changes will load on next start.`;
          daemonState = "down";
          daemonStatus = null;
        } else {
          error = `Daemon apply failed: ${message}`;
          daemonState = "down";
          daemonStatus = null;
        }
      }
    } catch (e) {
      error = String(e);
    } finally {
      applying = false;
    }
  }

  // Derive suggested values for the layout's color vars from the selected
  // background's dominant colors. Merging into `values` retriggers the live
  // preview, so the suggestion is visible immediately and adjustable before
  // Apply — nothing is persisted until the user applies/saves as usual.
  async function suggestColors() {
    if (!selectedLayout || !selectedBackground || suggesting) return;
    suggesting = true;
    status = "";
    error = "";
    try {
      const suggested = await invoke<Record<string, string>>("suggest_colors", {
        layout: selectedLayout,
        background: selectedBackground,
      });
      values = { ...values, ...suggested };
      status = `Suggested ${Object.keys(suggested).length} colors from ${selectedBackground}. Tweak freely, then Apply.`;
    } catch (e) {
      error = String(e);
    } finally {
      suggesting = false;
    }
  }

  function kindClass(kind: string): string {
    if (kind === "html") return "kind-html";
    if (kind === "xvfb") return "kind-xvfb";
    return "kind-svg";
  }

  function daemonLabel(): string {
    switch (daemonState) {
      case "ok":
        return daemonStatus?.connected === false ? "Daemon · No USB" : "Daemon · Online";
      case "down":
        return "Daemon · Offline";
      default:
        return "Daemon · Probing";
    }
  }

  function daemonClass(): string {
    if (daemonState === "ok") return daemonStatus?.connected === false ? "warn" : "ok";
    if (daemonState === "down") return "err";
    return "warn";
  }
</script>

<div class="app-shell">
  <header class="titlebar">
    <div class="brand-mark">
      <span class="glyph">&#x25c8;</span>
      <span>Thermalwriter</span>
      <span class="tag">// LCD HUD CONTROL</span>
    </div>

    <div class="titlebar-center">
      <span>&#x25b8; Peerless Vision · {titlebarResolution}</span>
      <span class="device-badge {deviceBadgeClass}">{deviceBadgeLabel}</span>
    </div>

    <div class="titlebar-right">
      <div class="theme-picker">
        <label for="theme-select">Theme</label>
        <select
          id="theme-select"
          value={theme}
          onchange={(e) => { theme = e.currentTarget.value as ThemeId; }}
        >
          {#each THEMES as t}
            <option value={t.id}>{t.label}</option>
          {/each}
        </select>
      </div>

      <div class="daemon-pill {daemonClass()}">
        <span class="led"></span>
        <span>{daemonLabel()}</span>
      </div>
    </div>
  </header>

  <main class="main-grid">
    <!-- ───────── Sidebar ───────── -->
    <section class="panel sidebar">
      <div class="panel-header">
        <div class="panel-title">
          <span class="marker">&#x276f;</span>
          <span>Layouts</span>
        </div>
      </div>
      <div class="panel-body">
        {#if loading}
          <div class="empty">Loading layouts…</div>
        {:else}
          {#if configurableLayouts.length > 0}
            <div class="section-label">
              <span>Configurable</span>
            </div>
            <div class="layout-list">
              {#each configurableLayouts as layout}
                <button
                  type="button"
                  class="layout-row {kindClass(layout.kind)}"
                  class:active={layout.name === selectedLayout}
                  class:active-daemon={layout.name === activeDaemonLayout}
                  onclick={() => selectLayout(layout.name)}
                >
                  <span class="kind-dot"></span>
                  <span class="name">{layout.name}</span>
                  <span class="meta">{layout.kind}</span>
                </button>
              {/each}
            </div>
          {/if}

          {#if previewOnlyLayouts.length > 0}
            <div class="section-label">
              <span>Preview only</span>
            </div>
            <div class="layout-list">
              {#each previewOnlyLayouts as layout}
                <button
                  type="button"
                  class="layout-row muted {kindClass(layout.kind)}"
                  class:active={layout.name === selectedLayout}
                  class:active-daemon={layout.name === activeDaemonLayout}
                  onclick={() => selectLayout(layout.name)}
                >
                  <span class="kind-dot"></span>
                  <span class="name">{layout.name}</span>
                  <span class="meta">{layout.kind}</span>
                </button>
              {/each}
            </div>
          {/if}

          <div class="section-label">
            <span>Background</span>
          </div>
          <BgGallery
            {backgrounds}
            selected={selectedBackground}
            onselect={(name) => { selectedBackground = name; }}
            onimported={(name) => refreshBackgrounds(name)}
          />
        {/if}
      </div>
    </section>

    <!-- ───────── Preview ───────── -->
    <section class="panel preview-pane">
      <div class="panel-header">
        <div class="panel-title">
          <span class="marker">&#x25c9;</span>
          <span>Live preview</span>
        </div>
        <div class="panel-title" style="color: var(--text-dim)">
          <span>{previewing ? "RENDER" : "READY"}</span>
        </div>
      </div>
      <div class="panel-body">
        <div class="canvas-wrap">
          <div class="canvas-frame">
            <canvas bind:this={canvas} width="480" height="480"></canvas>
          </div>
        </div>

        <div class="preview-meta">
          <span class="layout-name">{selectedLayout || "— no layout —"}</span>
          <span class="meta-mid">
            <span class="dot"></span>
            <span>{selected?.kind ?? "—"}</span>
            <span class="dot"></span>
            <span>{selected?.configurable ? "editable" : "preview-only"}</span>
          </span>
          <span class="render-status" class:busy={previewing}>
            {previewing ? "Rendering…" : "Idle"}
          </span>
        </div>
      </div>
    </section>

    <!-- ───────── Config / Stream ───────── -->
    <section class="panel config-pane">
      <div class="panel-header">
        <div class="panel-title">
          <span class="marker">&#x2699;</span>
          <nav class="tab-nav">
            <button
              type="button"
              class="tab-btn"
              class:active={activeTab === "variables"}
              onclick={() => { activeTab = "variables"; }}
            >Variables</button>
            <button
              type="button"
              class="tab-btn kind-xvfb"
              class:active={activeTab === "stream"}
              onclick={() => { activeTab = "stream"; }}
            >Stream</button>
          </nav>
        </div>
        {#if activeTab === "variables"}
          <div class="header-actions">
            <button
              type="button"
              class="btn-suggest"
              onclick={suggestColors}
              disabled={suggesting || !selectedBackground || !hasColorVars}
              title={!selectedBackground
                ? "Select a background to suggest colors from"
                : !hasColorVars
                  ? "This layout declares no color variables"
                  : "Suggest overlay colors from the background's dominant colors"}
            >
              {suggesting ? "Sampling…" : "◑ Suggest"}
            </button>
            <button
              type="button"
              class="btn-apply"
              onclick={apply}
              disabled={applying || !selectedLayout}
            >
              {applying ? "Applying…" : "Apply ↳"}
            </button>
          </div>
        {/if}
      </div>
      <div class="panel-body">
        {#if activeTab === "variables"}
          {#if !selectedLayout}
            <div class="empty">Select a layout to edit its variables.</div>
          {:else if variables.length === 0}
            <div class="empty">
              <strong>{selectedLayout}</strong> declares no editable variables. The layout
              renders as-authored; only the background can be changed.
            </div>
          {:else}
            <form class="var-list" onsubmit={(event) => event.preventDefault()}>
              {#each variables as variable}
                <div class="var-row">
                  <div class="var-row-head">
                    <span class="var-name">{variable.name}</span>
                    <span class="var-type">{variable.type}</span>
                  </div>
                  {#if variable.help}
                    <span class="var-help">{variable.help}</span>
                  {/if}
                  <div class="var-control">
                    {#if variable.type === "color"}
                      <input
                        type="color"
                        value={values[variable.name] ?? variable.default}
                        oninput={(event) => setValue(variable.name, event.currentTarget.value)}
                      />
                      <input
                        type="text"
                        class="color-hex"
                        value={values[variable.name] ?? variable.default}
                        oninput={(event) => setValue(variable.name, event.currentTarget.value)}
                      />
                    {:else if variable.type === "number"}
                      <div class="number-control">
                        {#if variable.min !== null && variable.max !== null}
                          <input
                            type="range"
                            min={variable.min}
                            max={variable.max}
                            step={variable.step ?? "any"}
                            value={values[variable.name] ?? variable.default}
                            oninput={(event) => setValue(variable.name, event.currentTarget.value)}
                          />
                        {/if}
                        <input
                          type="number"
                          class="number-field"
                          min={variable.min ?? undefined}
                          max={variable.max ?? undefined}
                          step={variable.step ?? "any"}
                          value={values[variable.name] ?? variable.default}
                          oninput={(event) => setValue(variable.name, event.currentTarget.value)}
                        />
                      </div>
                    {:else if variable.type === "sensor"}
                      <select
                        value={values[variable.name] ?? variable.default}
                        onchange={(event) => setValue(variable.name, event.currentTarget.value)}
                      >
                        {#each sensors as sensor}
                          <option value={sensor.key}>{sensor.name} ({sensor.unit})</option>
                        {/each}
                      </select>
                    {:else}
                      <input
                        type="text"
                        value={values[variable.name] ?? variable.default}
                        oninput={(event) => setValue(variable.name, event.currentTarget.value)}
                      />
                    {/if}
                  </div>
                </div>
              {/each}
            </form>
          {/if}

          {#if status}
            <p class="status">{status}</p>
          {/if}
          {#if error}
            <p class="error">{error}</p>
          {/if}
        {/if}

        <!-- Stream tab — always mounted so the poll loop and streaming state
             survive tab switches. Hidden with display:none when inactive so
             onDestroy only fires on full component teardown, not tab switches.
             This prevents the interval from being cleared and re-created on
             every Variables ↔ Stream switch. -->
        <div style:display={activeTab === "stream" ? "contents" : "none"}>
          <StreamTab
            {selectedLayout}
            onDaemonStateChange={probeDaemon}
            tabVisible={activeTab === "stream"}
          />
        </div>
      </div>
    </section>
  </main>

  <footer class="statusbar">
    <span>BUILD 0.1.0</span>
    <span class="sep"></span>
    <span>
      <span class={daemonClass()}>
        {daemonLabel()}
      </span>
    </span>
    <span class="sep"></span>
    <span>Layouts <span class="accent">{layouts.length}</span></span>
    <span class="sep"></span>
    <span>Sensors <span class="accent">{sensors.length}</span></span>
    <span class="right">{theme.toUpperCase().replace(/-/g, " · ")}</span>
  </footer>
</div>
