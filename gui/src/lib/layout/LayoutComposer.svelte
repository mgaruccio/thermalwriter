<script lang="ts">
  import ValidationPanel from "./ValidationPanel.svelte";
  import ModuleInspector from "./ModuleInspector.svelte";
  import ModuleList from "./ModuleList.svelte";
  import PresetStarter from "./PresetStarter.svelte";
  import type {
    ComposerSaveState,
    LayoutDiagnostic,
    LayoutDocument,
    LayoutModule,
    LayoutPreset,
    ModuleKind,
    ModuleReorderDirection,
    SensorDescriptor,
  } from "./types";

  type LayoutSummary = {
    name: string;
    kind: string;
    configurable: boolean;
  };

  type Props = {
    presets: LayoutPreset[];
    savedLayouts: LayoutSummary[];
    draft: LayoutDocument | null;
    sensors: SensorDescriptor[];
    diagnostics?: LayoutDiagnostic[];
    saveState: ComposerSaveState;
    previewing: boolean;
    previewValid: boolean | null;
    loading?: boolean;
    saving?: boolean;
    applying?: boolean;
    status?: string;
    error?: string;
    createFromPreset: (preset: LayoutPreset, name: string) => void;
    reopenDocument: (name: string) => void;
    renameDraft: (name: string) => void;
    addModule: (kind: ModuleKind) => void;
    removeModule: (id: string) => void;
    reorderModule: (id: string, direction: ModuleReorderDirection) => void;
    updateModule: (id: string, module: LayoutModule) => void;
    saveDraft: () => void;
    applyDraft: () => void;
  };

  let {
    presets,
    savedLayouts,
    draft,
    sensors,
    diagnostics = [],
    saveState,
    previewing,
    previewValid,
    loading = false,
    saving = false,
    applying = false,
    status = "",
    error = "",
    createFromPreset,
    reopenDocument,
    renameDraft,
    addModule,
    removeModule,
    reorderModule,
    updateModule,
    saveDraft,
    applyDraft,
  }: Props = $props();

  let selectedSavedLayout = $state("");
  const controlsDisabled = $derived(loading || saving || applying);
  const stateLabel = $derived(
    saveState === "active" ? "Active" : saveState === "saved" ? "Saved" : "Unsaved",
  );
  const previewLabel = $derived(
    previewing
      ? "Previewing"
      : previewValid === false
        ? "Needs attention"
        : previewValid === true
          ? "Preview ready"
          : "Preview waiting",
  );

  function reopenSelected() {
    if (selectedSavedLayout) reopenDocument(selectedSavedLayout);
  }

  let selectedModuleId = $state<string | null>(null);
  const selectedModule = $derived(
    draft?.modules.find((module) => module.id === selectedModuleId) ?? draft?.modules[0] ?? null,
  );
</script>

<div class="composer-view">
  <div class="composer-toolbar">
    <div>
      <p class="eyebrow">Layout studio</p>
      <h2>Compose a display</h2>
      <p class="composer-intro">
        Start from a flagship preset, then arrange bounded modules in the order the engine will solve them.
      </p>
    </div>
    <div class="composer-state-stack" aria-label="Composer state">
      <span class="composer-state save-state-{saveState}">{stateLabel}</span>
      <span class="composer-state preview-state" class:busy={previewing}>{previewLabel}</span>
    </div>
  </div>

  <PresetStarter {presets} oncreate={createFromPreset} busy={controlsDisabled} />

  <section class="composer-section reopen-section" aria-labelledby="reopen-layout-title">
    <div class="composer-section-heading">
      <div>
        <p class="eyebrow">Continue an existing document</p>
        <h2 id="reopen-layout-title">Reopen a saved layout</h2>
      </div>
      <span class="composer-count">{savedLayouts.length} saved</span>
    </div>
    <div class="reopen-controls">
      <label class="sr-only" for="saved-layout-select">Saved layout</label>
      <select id="saved-layout-select" bind:value={selectedSavedLayout} disabled={controlsDisabled || savedLayouts.length === 0}>
        <option value="">Choose a saved layout…</option>
        {#each savedLayouts as layout (layout.name)}
          <option value={layout.name}>{layout.name.replace(/\.layout\.toml$/i, "")}</option>
        {/each}
      </select>
      <button
        type="button"
        class="btn-secondary"
        onclick={reopenSelected}
        disabled={controlsDisabled || !selectedSavedLayout}
      >
        {loading ? "Opening…" : "Reopen"}
      </button>
    </div>
    {#if savedLayouts.length === 0}
      <small class="composer-help">Save your first composition and it will appear here for quick reopening.</small>
    {/if}
  </section>

  {#if draft}
    <section class="composer-section draft-details" aria-labelledby="draft-details-title">
      <div class="composer-section-heading">
        <div>
          <p class="eyebrow">Current draft</p>
          <h2 id="draft-details-title">Name and publish</h2>
        </div>
        <span class="composer-count">{draft.modules.length} modules</span>
      </div>
      <label class="composer-field" for="active-layout-name">
        <span>Layout name</span>
        <input
          id="active-layout-name"
          type="text"
          value={draft.name}
          oninput={(event) => renameDraft(event.currentTarget.value)}
          autocomplete="off"
          spellcheck="false"
          disabled={controlsDisabled}
        />
        <small>Use a short name for this saved composition. The engine adds the typed document suffix.</small>
      </label>
      <div class="composer-actions">
        <button
          type="button"
          class="btn-secondary"
          onclick={saveDraft}
          disabled={controlsDisabled || !draft.name.trim()}
        >
          {saving ? "Saving…" : "Save layout"}
        </button>
        <button
          type="button"
          class="btn-apply"
          onclick={applyDraft}
          disabled={controlsDisabled || !draft.name.trim()}
        >
          {applying ? "Activating…" : "Save & activate"}
        </button>
      </div>
    </section>

    <ModuleList
      modules={draft.modules}
      onadd={addModule}
      onremove={removeModule}
      onreorder={reorderModule}
      bind:selectedId={selectedModuleId}
      disabled={controlsDisabled}
    />
    <ModuleInspector
      module={selectedModule}
      sensors={sensors}
      profiles={draft.profiles}
      diagnostics={diagnostics}
      onchange={(module) => updateModule(module.id, module)}
      disabled={controlsDisabled}
    />
    <ValidationPanel diagnostics={diagnostics} />
  {:else}
    <div class="composer-empty">
      <span class="composer-empty-mark" aria-hidden="true">＋</span>
      <strong>Your composition will appear here</strong>
      <span>Choose the Neon Composer preset above to begin.</span>
    </div>
  {/if}

  {#if status}
    <p class="status" role="status">{status}</p>
  {/if}
  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}
</div>
