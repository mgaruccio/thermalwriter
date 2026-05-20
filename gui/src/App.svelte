<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import BgGallery from "./lib/BgGallery.svelte";

  type LayoutSummary = {
    name: string;
    kind: string;
    configurable: boolean;
  };

  type VariableDecl = {
    name: string;
    type: "color" | "text" | "sensor";
    default: string;
    help: string;
    value: string;
  };

  type SensorDescriptor = {
    key: string;
    name: string;
    unit: string;
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
  let status = $state("");
  let error = $state("");
  let canvas = $state<HTMLCanvasElement | undefined>();
  let previewTimer: number | undefined;
  let daemonState = $state<"unknown" | "ok" | "down">("unknown");
  let theme = $state<ThemeId>(
    (localStorage.getItem("tw-theme") as ThemeId) || "tokyo-night-storm",
  );

  const selected = $derived(layouts.find((layout) => layout.name === selectedLayout));
  const configurableLayouts = $derived(layouts.filter((l) => l.configurable));
  const previewOnlyLayouts = $derived(layouts.filter((l) => !l.configurable));

  $effect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem("tw-theme", theme);
  });

  onMount(async () => {
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
      // list_sensors silently falls back when daemon is down — probe more
      // directly so the connection pill reflects reality.
      await probeDaemon();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

  async function probeDaemon() {
    try {
      // get_active_background just reads config.toml; piggyback the call so
      // we don't add a roundtrip. Real liveness is set by the apply path,
      // where set_background returns a definitive answer.
      await invoke("get_active_background");
      daemonState = "ok";
    } catch {
      daemonState = "down";
    }
  }

  $effect(() => {
    selectedLayout;
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
      } catch (e) {
        const message = String(e);
        if (message.includes("daemon is not running")) {
          await invoke<void>("save_config", {
            layout: selectedLayout,
            vars: values,
          });
          await invoke<void>("save_background", { name: selectedBackground });
          status = `Saved ${selectedLayout}. Daemon offline — changes will load on next start.`;
          daemonState = "down";
        } else {
          error = `Daemon apply failed: ${message}`;
          daemonState = "down";
        }
      }
    } catch (e) {
      error = String(e);
    } finally {
      applying = false;
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
        return "Daemon · Online";
      case "down":
        return "Daemon · Offline";
      default:
        return "Daemon · Probing";
    }
  }

  function daemonClass(): string {
    if (daemonState === "ok") return "ok";
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
      &#x25b8; Peerless Vision · 480 × 480 · USB 0x87AD/0x70DB
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
                  onclick={() => selectLayout(layout.name)}
                >
                  <span class="kind-dot"></span>
                  <span class="name">{layout.name}</span>
                  <span class="meta">{layout.kind}</span>
                </button>
              {/each}
            </div>
          {/if}

          {#if backgrounds.length > 0}
            <div class="section-label">
              <span>Background</span>
            </div>
            <BgGallery
              {backgrounds}
              selected={selectedBackground}
              onselect={(name) => { selectedBackground = name; }}
            />
          {/if}
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

    <!-- ───────── Config ───────── -->
    <section class="panel config-pane">
      <div class="panel-header">
        <div class="panel-title">
          <span class="marker">&#x2699;</span>
          <span>Variables</span>
        </div>
        <button
          type="button"
          class="btn-apply"
          onclick={apply}
          disabled={applying || !selectedLayout}
        >
          {applying ? "Applying…" : "Apply ↳"}
        </button>
      </div>
      <div class="panel-body">
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
      </div>
    </section>
  </main>

  <footer class="statusbar">
    <span>BUILD 0.1.0</span>
    <span class="sep"></span>
    <span>
      <span class={daemonState === "ok" ? "ok" : daemonState === "down" ? "warn" : ""}>
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
