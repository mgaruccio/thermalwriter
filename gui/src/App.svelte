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

  const selected = $derived(layouts.find((layout) => layout.name === selectedLayout));

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
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

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
      // Always persist first: save_config writes both [layout_vars."<name>"]
      // and [display].default_layout, so the choice survives a daemon
      // restart even though the daemon's in-memory set_layout doesn't
      // touch default_layout on its own.
      await invoke<void>("save_config", {
        layout: selectedLayout,
        vars: values,
      });
      // Persist background selection independently — survives daemon restarts
      // even if the live apply below fails.
      await invoke<void>("save_background", { name: selectedBackground });

      // Then ask the running daemon to switch live. If the daemon isn't
      // running we still kept the user's edits via save_config above.
      try {
        await invoke<void>("apply_to_daemon", {
          layout: selectedLayout,
          vars: values,
        });
        // Apply background live — separate D-Bus call, daemon-only.
        await invoke<void>("set_background", { name: selectedBackground });
        status = `Applied ${selectedLayout} to daemon.`;
      } catch (e) {
        const message = String(e);
        if (message.includes("daemon is not running")) {
          status = `Saved ${selectedLayout}. Daemon is not running; changes will apply on next start.`;
        } else {
          error = `Saved, but daemon apply failed: ${message}`;
        }
      }
    } catch (e) {
      error = String(e);
    } finally {
      applying = false;
    }
  }
</script>

<main class="app-shell">
  <aside class="sidebar">
    <div class="brand">
      <h1>Thermalwriter</h1>
      <p>Layout Config</p>
    </div>

    <div class="layout-list">
      {#if loading}
        <div class="empty">Loading layouts...</div>
      {:else}
        {#each layouts as layout}
          <button
            type="button"
            class:active={layout.name === selectedLayout}
            class:muted={!layout.configurable}
            onclick={() => selectLayout(layout.name)}
          >
            <span>{layout.name}</span>
            <small>{layout.kind}{layout.configurable ? "" : " · no vars"}</small>
          </button>
        {/each}
      {/if}
    </div>

    {#if !loading && backgrounds.length > 0}
      <div class="sidebar-section">
        <h3>Background</h3>
        <BgGallery
          {backgrounds}
          selected={selectedBackground}
          onselect={(name) => { selectedBackground = name; }}
        />
      </div>
    {/if}
  </aside>

  <section class="preview-pane">
    <div class="preview-header">
      <div>
        <h2>{selectedLayout || "No layout selected"}</h2>
        <p>{selected?.configurable ? "Editable SVG layout" : "Preview only"}</p>
      </div>
      <span class:busy={previewing}>{previewing ? "Rendering" : "Ready"}</span>
    </div>
    <div class="canvas-wrap">
      <canvas bind:this={canvas} width="480" height="480"></canvas>
    </div>
  </section>

  <aside class="config-pane">
    <div class="config-header">
      <h2>Variables</h2>
      <button type="button" onclick={apply} disabled={applying || !selectedLayout}>
        {applying ? "Applying..." : "Apply"}
      </button>
    </div>

    {#if variables.length === 0}
      <div class="empty">This layout does not declare editable variables.</div>
    {:else}
      <form class="var-list" onsubmit={(event) => event.preventDefault()}>
        {#each variables as variable}
          <label class="var-row">
            <span>
              <strong>{variable.name}</strong>
              <small>{variable.help}</small>
            </span>
            {#if variable.type === "color"}
              <input
                type="color"
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
          </label>
        {/each}
      </form>
    {/if}

    {#if status}
      <p class="status">{status}</p>
    {/if}
    {#if error}
      <p class="error">{error}</p>
    {/if}
  </aside>
</main>
