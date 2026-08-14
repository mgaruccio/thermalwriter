<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import BgGallery from "./lib/BgGallery.svelte";
  import StreamTab from "./lib/StreamTab.svelte";
  import LayoutComposer from "./lib/layout/LayoutComposer.svelte";
  import LayoutPreview from "./lib/layout/LayoutPreview.svelte";
  import {
    COMPOSER_PRESETS,
    PREVIEW_PROFILES,
    createModule,
    normalizeLayoutName,
    type ComposerSaveState,
    type LayoutApplyResponse,
    type LayoutDiagnostic,
    type LayoutDocument,
    type LayoutDocumentResponse,
    type LayoutModule,
    type LayoutPreviewResponse,
    type LayoutPreset,
    type LayoutSaveResponse,
    type ModuleKind,
    type ModuleReorderDirection,
    type PreviewProfile,
    type PreviewProfileId,
    type SensorDescriptor,
  } from "./lib/layout/types";
  import { bumpRevision, isCurrentRevision } from "./lib/asyncSelection";

  // Canonical prefix of AppError::DaemonUnavailable's serialized Display string.
  // AppError serializes to a plain string; this prefix is guaranteed by the
  // thiserror #[error("daemon is not running…")] template in error.rs.
  const DAEMON_OFFLINE_PREFIX = "daemon is not running";
  const DAEMON_STATUS_POLL_MS = 5000;

  function formatInvokeError(error: unknown): string {
    if (typeof error === "string") return error;
    if (error && typeof error === "object") {
      const rec = error as Record<string, unknown>;
      if (rec.kind === "layout-diagnostics" && Array.isArray(rec.diagnostics)) {
        return rec.diagnostics
          .map((item) => {
            const diagnostic = item as LayoutDiagnostic;
            return [`${diagnostic.code}: ${diagnostic.message}`, diagnostic.reason, diagnostic.fix]
              .filter(Boolean)
              .join("\n");
          })
          .join("\n\n");
      }
      if (typeof rec.message === "string" && rec.message.trim()) return rec.message;
      try {
        return JSON.stringify(error);
      } catch {
        /* fall through */
      }
    }
    return String(error);
  }

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
  let layoutSelectionRev = 0;
  let previewRequestRev = 0;
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
  let activeTab = $state<"variables" | "stream" | "compose">("variables");
  let composerDraft = $state<LayoutDocument | null>(null);
  let composerPreviewProfile = $state<PreviewProfileId>("square");
  let composerFingerprint = $state<string | null>(null);
  let composerSavedName = $state<string | null>(null);
  let composerSaveState = $state<ComposerSaveState>("unsaved");
  let composerPreview = $state<LayoutPreviewResponse | null>(null);
  let composerPreviewing = $state(false);
  let composerLoading = $state(false);
  let composerSaving = $state(false);
  let composerApplying = $state(false);
  let composerStatus = $state("");
  let composerError = $state("");
  let composerLoadRevision = 0;
  let composerDraftRevision = 0;
  let composerPreviewRevision = 0;
  let composerPreviewTimer: number | undefined;
  let theme = $state<ThemeId>(
    (localStorage.getItem("tw-theme") as ThemeId) || "tokyo-night-storm",
  );

  const selected = $derived(layouts.find((layout) => layout.name === selectedLayout));
  const hasColorVars = $derived(variables.some((variable) => variable.type === "color"));
  const configurableLayouts = $derived(layouts.filter((l) => l.configurable));
  const previewOnlyLayouts = $derived(layouts.filter((l) => !l.configurable));

  const composerLayouts = $derived(layouts.filter((layout) => layout.kind === "layout"));
  const composerProfile = $derived(
    PREVIEW_PROFILES.find((profile) => profile.id === composerPreviewProfile) ?? PREVIEW_PROFILES[0],
  );
  const composerPreviewValid = $derived(
    composerPreview
      ? composerPreview.diagnostics.every((diagnostic) => diagnostic.severity !== "error") &&
        composerPreview.rgba.length > 0
      : null,
  );

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
    if (composerPreviewTimer !== undefined) {
      window.clearTimeout(composerPreviewTimer);
      composerPreviewTimer = undefined;
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

  $effect(() => {
    const draftSnapshot = composerDraft;
    const profile = composerProfile;
    JSON.stringify(draftSnapshot);
    scheduleComposerPreview(draftSnapshot, profile);
  });

  function selectComposerProfile(profile: PreviewProfile) {
    composerPreviewProfile = profile.id;
    // Invalidate an in-flight response immediately, before the debounced
    // request for the new native surface begins.
    composerPreviewRevision = bumpRevision(composerPreviewRevision);
    composerPreview = null;
    composerError = "";
  }

  async function selectLayout(name: string) {
    const rev = (layoutSelectionRev = bumpRevision(layoutSelectionRev));
    selectedLayout = name;
    status = "";
    error = "";
    const decls = await invoke<VariableDecl[]>("get_layout_vars", { layout: name });
    if (!isCurrentRevision(rev, layoutSelectionRev)) return;
    variables = decls;
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


  function commitComposerDraft(nextDraft: LayoutDocument) {
    composerDraft = nextDraft;
    composerDraftRevision = bumpRevision(composerDraftRevision);
    composerSaveState = "unsaved";
    const nextName = normalizeLayoutName(nextDraft.name);
    if (composerSavedName !== nextName) composerFingerprint = null;
    composerStatus = "";
    composerError = "";
    composerPreview = null;
  }

  function renameDraft(name: string) {
    if (!composerDraft || composerDraft.name === name) return;
    commitComposerDraft({ ...composerDraft, name });
  }

  function addModule(kind: ModuleKind) {
    if (!composerDraft) return;
    const module = createModule(kind, composerDraft.modules);
    commitComposerDraft({ ...composerDraft, modules: [...composerDraft.modules, module] });
  }

  function removeModule(id: string) {
    if (!composerDraft || !composerDraft.modules.some((module) => module.id === id)) return;
    commitComposerDraft({
      ...composerDraft,
      modules: composerDraft.modules.filter((module) => module.id !== id),
    });
  }

  function updateComposerModule(id: string, nextModule: LayoutModule) {
    const currentDraft = composerDraft;
    if (!currentDraft || nextModule.id !== id) return;
    const modules = currentDraft.modules.map((module) => (module.id === id ? nextModule : module));
    if (modules.every((module, index) => module === currentDraft.modules[index])) return;
    commitComposerDraft({ ...currentDraft, modules });
  }

  function reorderModule(id: string, direction: ModuleReorderDirection) {
    if (!composerDraft) return;
    const modules = [...composerDraft.modules];
    const index = modules.findIndex((module) => module.id === id);
    const destination = direction === "up" ? index - 1 : index + 1;
    if (index < 0 || destination < 0 || destination >= modules.length) return;
    [modules[index], modules[destination]] = [modules[destination], modules[index]];
    commitComposerDraft({ ...composerDraft, modules });
  }

  function rememberSavedLayout(name: string) {
    const filename = `${name}.layout.toml`;
    if (layouts.some((layout) => layout.name === filename)) return;
    layouts = [...layouts, { name: filename, kind: "layout", configurable: false }];
  }

  async function createFromPreset(preset: LayoutPreset, requestedName: string) {
    const revision = (composerLoadRevision = bumpRevision(composerLoadRevision));
    const draftRevision = composerDraftRevision;
    composerLoading = true;
    composerStatus = `Loading ${preset.label}…`;
    composerError = "";
    try {
      const response = await invoke<LayoutDocumentResponse>("load_layout_preset", {
        preset: preset.id,
      });
      if (
        !isCurrentRevision(revision, composerLoadRevision) ||
        !isCurrentRevision(draftRevision, composerDraftRevision)
      ) {
        return;
      }
      const name = normalizeLayoutName(requestedName) || response.document.name || preset.id;
      composerDraft = { ...response.document, name };
      composerFingerprint = null;
      composerSavedName = null;
      composerSaveState = "unsaved";
      composerDraftRevision = bumpRevision(composerDraftRevision);
      composerPreviewRevision = bumpRevision(composerPreviewRevision);
      composerPreview = null;
      composerStatus = `Created ${name} from ${preset.label}. Arrange modules, then save when ready.`;
    } catch (e) {
      if (isCurrentRevision(revision, composerLoadRevision)) composerError = formatInvokeError(e);
    } finally {
      if (isCurrentRevision(revision, composerLoadRevision)) composerLoading = false;
    }
  }

  async function reopenDocument(name: string) {
    const revision = (composerLoadRevision = bumpRevision(composerLoadRevision));
    const draftRevision = composerDraftRevision;
    composerLoading = true;
    composerStatus = `Opening ${name.replace(/\.layout\.toml$/i, "")}…`;
    composerError = "";
    try {
      const response = await invoke<LayoutDocumentResponse>("load_layout_document", { name });
      if (
        !isCurrentRevision(revision, composerLoadRevision) ||
        !isCurrentRevision(draftRevision, composerDraftRevision)
      ) {
        return;
      }
      const savedName = normalizeLayoutName(response.document.name || name);
      composerDraft = { ...response.document, name: savedName };
      composerFingerprint = response.document_fingerprint;
      composerSavedName = savedName;
      composerSaveState = "saved";
      composerDraftRevision = bumpRevision(composerDraftRevision);
      composerPreviewRevision = bumpRevision(composerPreviewRevision);
      composerPreview = null;
      composerStatus = `Reopened ${savedName}. Module order is preserved from disk.`;
    } catch (e) {
      if (isCurrentRevision(revision, composerLoadRevision)) composerError = formatInvokeError(e);
    } finally {
      if (isCurrentRevision(revision, composerLoadRevision)) composerLoading = false;
    }
  }

  function scheduleComposerPreview(
    draftSnapshot: LayoutDocument | null,
    profile: PreviewProfile,
  ) {
    if (composerPreviewTimer !== undefined) window.clearTimeout(composerPreviewTimer);
    composerPreviewTimer = undefined;
    if (activeTab !== "compose" || !draftSnapshot) return;
    composerPreviewTimer = window.setTimeout(() => {
      composerPreviewTimer = undefined;
      void renderComposerPreview(draftSnapshot, profile);
    }, 120);
  }

  async function renderComposerPreview(draftSnapshot: LayoutDocument, profile: PreviewProfile) {
    const revision = (composerPreviewRevision = bumpRevision(composerPreviewRevision));
    const draftRevision = composerDraftRevision;
    composerPreviewing = true;
    composerError = "";
    try {
      const response = await invoke<LayoutPreviewResponse>("preview_layout_document", {
        draft: draftSnapshot,
        profile: profile.backendProfile,
        width: profile.width,
        height: profile.height,
      });
      if (
        !isCurrentRevision(revision, composerPreviewRevision) ||
        !isCurrentRevision(draftRevision, composerDraftRevision)
      ) {
        return;
      }
      composerPreview = response;
    } catch (e) {
      if (isCurrentRevision(revision, composerPreviewRevision)) {
        composerPreview = null;
        composerError = formatInvokeError(e);
      }
    } finally {
      if (isCurrentRevision(revision, composerPreviewRevision)) composerPreviewing = false;
    }
  }

  async function saveDraft() {
    const draftSnapshot = composerDraft;
    if (!draftSnapshot || composerSaving || composerApplying) return;
    const name = normalizeLayoutName(draftSnapshot.name);
    if (!name) {
      composerError = "Give this layout a name before saving.";
      return;
    }
    const revision = composerDraftRevision;
    const expectedFingerprint = composerSavedName === name ? composerFingerprint : null;
    composerSaving = true;
    composerStatus = "Saving typed layout…";
    composerError = "";
    try {
      const response = await invoke<LayoutSaveResponse>("save_layout_document", {
        name,
        expected_fingerprint: expectedFingerprint,
        draft: { ...draftSnapshot, name },
      });
      if (!isCurrentRevision(revision, composerDraftRevision)) return;
      composerDraft = { ...draftSnapshot, name: response.name };
      composerPreviewRevision = bumpRevision(composerPreviewRevision);
      composerFingerprint = response.document_fingerprint;
      composerSavedName = response.name;
      composerSaveState = "saved";
      composerStatus = `Saved ${response.name}. You can reopen it any time.`;
      rememberSavedLayout(response.name);
    } catch (e) {
      if (isCurrentRevision(revision, composerDraftRevision)) composerError = formatInvokeError(e);
    } finally {
      if (isCurrentRevision(revision, composerDraftRevision)) composerSaving = false;
    }
  }

  async function applyDraft() {
    const draftSnapshot = composerDraft;
    if (!draftSnapshot || composerSaving || composerApplying) return;
    const name = normalizeLayoutName(draftSnapshot.name);
    if (!name) {
      composerError = "Give this layout a name before activating.";
      return;
    }
    const revision = composerDraftRevision;
    const expectedFingerprint = composerSavedName === name ? composerFingerprint : null;
    composerApplying = true;
    composerStatus = "Saving and activating typed layout…";
    composerError = "";
    try {
      const response = await invoke<LayoutApplyResponse>("apply_layout_document", {
        name,
        expected_fingerprint: expectedFingerprint,
        draft: { ...draftSnapshot, name },
      });
      if (!isCurrentRevision(revision, composerDraftRevision)) return;
      composerDraft = { ...draftSnapshot, name: response.saved.name };
      composerPreviewRevision = bumpRevision(composerPreviewRevision);
      composerFingerprint = response.saved.document_fingerprint;
      composerSavedName = response.saved.name;
      rememberSavedLayout(response.saved.name);
      if (response.activation.state === "active") {
        composerSaveState = "active";
        composerStatus = `Active on the device: ${response.saved.name}.`;
      } else {
        composerSaveState = "saved";
        composerStatus = `Saved ${response.saved.name}, but activation was not completed: ${response.activation.reason}`;
      }
    } catch (e) {
      if (isCurrentRevision(revision, composerDraftRevision)) composerError = formatInvokeError(e);
    } finally {
      if (isCurrentRevision(revision, composerDraftRevision)) composerApplying = false;
    }
  }

  function schedulePreview() {
    if (!selectedLayout || !canvas) return;
    if (previewTimer) window.clearTimeout(previewTimer);
    previewTimer = window.setTimeout(renderPreview, 120);
  }

  async function renderPreview() {
    if (!selectedLayout || !canvas) return;
    const rev = (previewRequestRev = bumpRevision(previewRequestRev));
    const layout = selectedLayout;
    const varsSnapshot = { ...values };
    const background = selectedBackground;
    previewing = true;
    error = "";
    try {
      const buffer = await invoke<ArrayBuffer>("render_preview", {
        layout,
        vars: varsSnapshot,
        background,
      });
      if (!isCurrentRevision(rev, previewRequestRev) || layout !== selectedLayout) return;
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("Canvas context unavailable");
      const image = new ImageData(new Uint8ClampedArray(buffer), 480, 480);
      ctx.putImageData(image, 0, 0);
    } catch (e) {
      if (!isCurrentRevision(rev, previewRequestRev) || layout !== selectedLayout) return;
      error = String(e);
    } finally {
      if (isCurrentRevision(rev, previewRequestRev)) previewing = false;
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

  function displayLayoutName(name: string): string {
    return name.replace(/\.(?:layout\.toml|svg|html)$/i, "");
  }

  function layoutKindLabel(kind: string): string {
    if (kind === "layout") return "Composer";
    if (kind === "svg") return "Display";
    if (kind === "html") return "Web";
    if (kind === "xvfb") return "Stream";
    return kind;
  }

  function formatSensorCost(costUs: number): string {
    if (!costUs || costUs <= 0) return "~0 µs";
    if (costUs < 1000) return `${Math.round(costUs)} µs`;
    return `${(costUs / 1000).toFixed(2)} ms`;
  }

  function sensorOptionLabel(sensor: SensorDescriptor): string {
    const unit = sensor.unit ? ` (${sensor.unit})` : "";
    return `${sensor.name}${unit} · ${formatSensorCost(sensor.cost_us)}`;
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
      <span class="tag">Display studio</span>
    </div>

    <div class="titlebar-center">
      <span>Peerless Vision · {titlebarResolution}</span>
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
                  <span class="name">{displayLayoutName(layout.name)}</span>
                  <span class="meta">{layoutKindLabel(layout.kind)}</span>
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
                  <span class="name">{displayLayoutName(layout.name)}</span>
                  <span class="meta">{layoutKindLabel(layout.kind)}</span>
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

    <!-- ───────── Preview pane ───────── -->
    <section class="panel preview-pane">
      <div class="panel-header">
        <div class="panel-title">
          <span class="marker">&#x25c9;</span>
          <span>{activeTab === "compose" ? "Composition preview" : "Live preview"}</span>
        </div>
        <div class="panel-title" style="color: var(--text-dim)">
          <span>
            {activeTab === "compose"
              ? composerPreviewing
                ? "PREVIEWING"
                : composerPreviewValid === false
                  ? "CHECK"
                  : composerPreviewValid === null
                    ? "WAITING"
                    : "READY"
              : previewing
                ? "RENDER"
                : "READY"}
          </span>
        </div>
      </div>
      <div class="panel-body">
        {#if activeTab === "compose"}
          <LayoutPreview
            preview={composerPreview}
            previewing={composerPreviewing}
            draftName={composerDraft?.name ?? ""}
            profileLabel={composerProfile.label}
            saveState={composerSaveState}
            nativeDimensionsAvailable={true}
            error={composerError}
          />
        {:else}
          <div class="canvas-wrap">
            <div class="canvas-frame">
              <canvas bind:this={canvas} width="480" height="480"></canvas>
            </div>
          </div>
          <div class="preview-meta">
            <span class="layout-name">{selectedLayout ? displayLayoutName(selectedLayout) : "— no layout —"}</span>
            <span class="meta-mid">
              <span class="dot"></span>
              <span>{layoutKindLabel(selected?.kind ?? "—")}</span>
              <span class="dot"></span>
              <span>{selected?.configurable ? "editable" : "preview-only"}</span>
            </span>
            <span class="render-status" class:busy={previewing}>
              {previewing ? "Rendering…" : "Idle"}
            </span>
          </div>
        {/if}
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
              class="tab-btn kind-layout"
              class:active={activeTab === "compose"}
              onclick={() => { activeTab = "compose"; }}
            >Compose</button>
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
              {suggesting ? "Sampling…" : "Suggest colors"}
            </button>
            <button
              type="button"
              class="btn-apply"
              onclick={apply}
              disabled={applying || !selectedLayout}
            >
              {applying ? "Applying…" : "Apply"}
            </button>
          </div>
        {/if}
      </div>
      <div class="panel-body">
        {#if activeTab === "compose"}
          <LayoutComposer
            presets={COMPOSER_PRESETS}
            savedLayouts={composerLayouts}
            draft={composerDraft}
            bind:previewProfile={composerPreviewProfile}
            onpreviewprofilechange={selectComposerProfile}
            sensors={sensors}
            diagnostics={composerPreview?.diagnostics ?? []}
            saveState={composerSaveState}
            previewing={composerPreviewing}
            previewValid={composerPreviewValid}
            loading={composerLoading}
            saving={composerSaving}
            applying={composerApplying}
            status={composerStatus}
            error={composerError}
            createFromPreset={createFromPreset}
            reopenDocument={reopenDocument}
            renameDraft={renameDraft}
            addModule={addModule}
            removeModule={removeModule}
            reorderModule={reorderModule}
            updateModule={updateComposerModule}
            saveDraft={saveDraft}
            applyDraft={applyDraft}
          />
        {:else if activeTab === "variables"}
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
                          <option value={sensor.key}>{sensorOptionLabel(sensor)}</option>
                        {/each}
                      </select>
                      {#if sensors.some((s) => s.cost_us > 0)}
                        <span class="var-help">
                          Poll cost is live-measured on this machine. Prefer low-µs keys for cooler layouts.
                        </span>
                      {/if}
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
