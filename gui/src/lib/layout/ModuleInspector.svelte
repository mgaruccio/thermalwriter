<script lang="ts">
  import {
    hasRelevantBridgeProfile,
    isKnownModuleBinding,
    moduleBinding,
    moduleBindingKind,
    moduleBindingOptions,
    moduleCapabilitiesFor,
    moduleKindLabel,
    type LayoutDiagnostic,
    type LayoutDocument,
    type LayoutModule,
    type MediaDocument,
    type SensorDescriptor,
  } from "./types";

  type Props = {
    module: LayoutModule | null;
    sensors: SensorDescriptor[];
    profiles?: LayoutDocument["profiles"];
    diagnostics?: LayoutDiagnostic[];
    onchange: (module: LayoutModule) => void;
    disabled?: boolean;
  };

  let {
    module,
    sensors,
    profiles,
    diagnostics = [],
    onchange,
    disabled = false,
  }: Props = $props();

  const capability = $derived(module ? moduleCapabilitiesFor(module.kind) : null);
  const binding = $derived(module ? moduleBinding(module) : "");
  const bindingOptions = $derived(module ? moduleBindingOptions(module.kind, sensors) : []);
  const bindingKind = $derived(module ? moduleBindingKind(module.kind) : null);
  const bindingKnown = $derived(
    !module || bindingKind === "media" || isKnownModuleBinding(module.kind, binding, sensors),
  );
  const bridgeAvailable = $derived(Boolean(module?.kind === "media" && hasRelevantBridgeProfile(profiles)));
  const moduleDiagnostics = $derived.by(() => {
    if (!module) return [];

    const local: LayoutDiagnostic[] = [];
    if (bindingKind !== "media") {
      if (!binding.trim()) {
        local.push({
          code: "TWGUI-BINDING-EMPTY",
          severity: "error",
          message: "This module needs a binding",
          module_id: module.id,
          property_path: "binding",
          reason: "The selected typed module cannot render without a supported runtime key.",
          fix: "Choose a sensor or history from the binding picker.",
        });
      } else if (!bindingKnown) {
        local.push({
          code: "TWGUI-BINDING-MISSING",
          severity: "warning",
          message: "Binding is no longer in the sensor catalog",
          module_id: module.id,
          property_path: "binding",
          reason: `The current catalog does not contain ${binding}.`,
          fix: "Choose an available sensor so the preview and saved document stay portable.",
        });
      }
    }

    if (capability && capability.variants.length > 0 && !capability.variants.some((option) => option.value === module.variant)) {
      local.push({
        code: "TWGUI-VARIANT-UNKNOWN",
        severity: "warning",
        message: "Presentation variant is unavailable",
        module_id: module.id,
        property_path: "variant",
        reason: `The typed ${module.kind} module does not advertise ${module.variant || "an empty variant"}.`,
        fix: "Choose one of the curated presentation variants.",
      });
    }

    const opacityMetadata = capability?.opacity;
    if (module.kind === "media" && opacityMetadata) {
      const opacity = module.opacity ?? 1;
      if (!Number.isFinite(opacity) || opacity < opacityMetadata.min || opacity > opacityMetadata.max) {
        local.push({
          code: "TWGUI-MEDIA-OPACITY",
          severity: "error",
          message: "Media opacity is outside the readable range",
          module_id: module.id,
          property_path: "opacity",
          reason: `The current value must stay between ${opacityMetadata.min} and ${opacityMetadata.max}.`,
          fix: "Use the bounded opacity control.",
        });
      }
    }

    const previewDiagnostics = diagnostics.filter((diagnostic) => diagnostic.module_id === module.id);
    return [...local, ...previewDiagnostics];
  });

  function eventValue(event: Event): string {
    return (event.currentTarget as HTMLInputElement | HTMLSelectElement).value;
  }

  function eventChecked(event: Event): boolean {
    return (event.currentTarget as HTMLInputElement).checked;
  }

  function formatSensorCost(costUs: number): string {
    return costUs > 0 ? `${costUs} µs poll` : "catalog";
  }

  function updateBinding(value: string) {
    if (!module || bindingKind === "media") return;
    onchange({ ...module, binding: value });
  }

  function updateVariant(value: string) {
    if (!module) return;
    onchange({ ...module, variant: value });
  }

  function updateMediaSource(value: string) {
    if (!module || module.kind !== "media") return;
    const next: MediaDocument = { ...module, binding: value, source: value };
    onchange(next);
  }

  function updateMediaFit(value: string) {
    if (!module || module.kind !== "media") return;
    if (value !== "contain" && value !== "cover") return;
    onchange({ ...module, fit: value });
  }

  function updateMediaOpacity(value: string) {
    if (!module || module.kind !== "media" || !capability?.opacity) return;
    const numeric = Number(value);
    if (!Number.isFinite(numeric)) return;
    const bounded = Math.min(capability.opacity.max, Math.max(capability.opacity.min, numeric));
    onchange({ ...module, opacity: bounded });
  }

  function updateBridgeSpan(enabled: boolean) {
    if (!module || module.kind !== "media" || !bridgeAvailable) return;
    onchange({ ...module, span_bridge: enabled });
  }

  function optionHelp(options: readonly { value: string; help: string }[], value: string): string {
    return options.find((option) => option.value === value)?.help ?? "Choose a curated option.";
  }

  function fieldId(prefix: string): string {
    return `module-${module?.id ?? "none"}-${prefix}`.replace(/[^a-zA-Z0-9_-]/g, "-");
  }
</script>

{#if module}
  <section class="composer-section module-inspector" aria-labelledby={fieldId("title")}>
    <div class="composer-section-heading">
      <div>
        <p class="eyebrow">Typed inspector</p>
        <h2 id={fieldId("title")}>Configure {moduleKindLabel(module.kind)}</h2>
      </div>
      <span class="composer-count">Stable ID · {module.id}</span>
    </div>

    {#if bindingKind !== "media"}
      <div class="inspector-field">
        <label for={fieldId("binding")}>
          <span>{capability?.bindingLabel ?? "Binding"}</span>
          <span class="inspector-unit">{bindingKind === "history" ? "history" : "sensor"}</span>
        </label>
        <select
          id={fieldId("binding")}
          value={binding}
          onchange={(event) => updateBinding(eventValue(event))}
          aria-describedby={fieldId("binding-help")}
          aria-invalid={!bindingKnown}
          disabled={disabled}
        >
          <option value="">Choose a supported binding…</option>
          {#if binding && !bindingKnown}
            <option value={binding}>{binding} · unavailable</option>
          {/if}
          {#each bindingOptions as sensor (sensor.key)}
            <option value={sensor.key}>{sensor.name}{sensor.unit ? ` (${sensor.unit})` : ""} · {formatSensorCost(sensor.cost_us)}</option>
          {/each}
        </select>
        <small id={fieldId("binding-help")}>{capability?.bindingHelp}</small>
      </div>
    {:else if module.kind === "media"}
      <div class="inspector-field">
        <label for={fieldId("source")}>
          <span>{capability?.bindingLabel ?? "Media source"}</span>
          <span class="inspector-unit">relative path</span>
        </label>
        <input
          id={fieldId("source")}
          type="text"
          maxlength="240"
          value={module.source || module.binding}
          oninput={(event) => updateMediaSource(eventValue(event))}
          aria-describedby={fieldId("source-help")}
          disabled={disabled}
          autocomplete="off"
          spellcheck="false"
        />
        <small id={fieldId("source-help")}>{capability?.bindingHelp} The engine rejects paths outside that directory.</small>
      </div>
    {/if}

    {#if capability && capability.variants.length > 0}
      <div class="inspector-field">
        <label for={fieldId("variant")}>
          <span>{module.kind === "text" ? "Text role" : "Presentation variant"}</span>
          <span class="inspector-unit">curated</span>
        </label>
        <select
          id={fieldId("variant")}
          value={module.variant}
          onchange={(event) => updateVariant(eventValue(event))}
          aria-describedby={fieldId("variant-help")}
          aria-invalid={!capability.variants.some((option) => option.value === module.variant)}
          disabled={disabled}
        >
          {#if module.variant && !capability.variants.some((option) => option.value === module.variant)}
            <option value={module.variant}>{module.variant} · unavailable</option>
          {/if}
          {#each capability.variants as option (option.value)}
            <option value={option.value}>{option.label}</option>
          {/each}
        </select>
        <small id={fieldId("variant-help")}>{optionHelp(capability.variants, module.variant)}</small>
      </div>
    {/if}

    {#if module.kind === "media" && capability?.fitOptions}
      <div class="inspector-field">
        <label for={fieldId("fit")}>
          <span>Image fit</span>
          <span class="inspector-unit">bounded</span>
        </label>
        <select
          id={fieldId("fit")}
          value={module.fit ?? "contain"}
          onchange={(event) => updateMediaFit(eventValue(event))}
          aria-describedby={fieldId("fit-help")}
          disabled={disabled}
        >
          {#each capability.fitOptions as option (option.value)}
            <option value={option.value}>{option.label}</option>
          {/each}
        </select>
        <small id={fieldId("fit-help")}>{optionHelp(capability.fitOptions, module.fit ?? "contain")}</small>
      </div>
    {/if}

    {#if module.kind === "media" && capability?.opacity}
      <div class="inspector-field">
        <label for={fieldId("opacity")}>
          <span>Media opacity</span>
          <span class="inspector-unit">{capability.opacity.unit} · min {capability.opacity.min}</span>
        </label>
        <div class="inspector-range">
          <input
            id={fieldId("opacity")}
            type="range"
            min={capability.opacity.min}
            max={capability.opacity.max}
            step={capability.opacity.step}
            value={module.opacity ?? capability.opacity.max}
            oninput={(event) => updateMediaOpacity(eventValue(event))}
            aria-describedby={fieldId("opacity-help")}
            disabled={disabled}
          />
          <output for={fieldId("opacity")}>{(module.opacity ?? capability.opacity.max).toFixed(2)}</output>
        </div>
        <small id={fieldId("opacity-help")}>{capability.opacity.help}</small>
      </div>
    {/if}

    {#if module.kind === "media" && bridgeAvailable}
      <div class="inspector-check-field">
        <label for={fieldId("span-bridge")}>
          <input
            id={fieldId("span-bridge")}
            type="checkbox"
            checked={module.span_bridge ?? false}
            onchange={(event) => updateBridgeSpan(eventChecked(event))}
            aria-describedby={fieldId("span-bridge-help")}
            disabled={disabled}
          />
          <span>Allow bridge span</span>
        </label>
        <small id={fieldId("span-bridge-help")}>{capability?.bridgeHelp}</small>
      </div>
    {/if}

    {#if moduleDiagnostics.length > 0}
      <div class="inspector-diagnostics" role="status" aria-live="polite">
        {#each moduleDiagnostics as diagnostic (diagnostic.code + (diagnostic.property_path ?? ""))}
          <div class="inspector-diagnostic diagnostic-{diagnostic.severity}">
            <strong>{diagnostic.message}</strong>
            <span>{diagnostic.reason}</span>
            <small>{diagnostic.fix}</small>
          </div>
        {/each}
      </div>
    {/if}
  </section>
{:else}
  <section class="composer-section module-inspector module-inspector-empty" aria-labelledby="module-inspector-empty-title">
    <p class="eyebrow">Typed inspector</p>
    <h2 id="module-inspector-empty-title">Select a module to configure it</h2>
    <p>Choose Configure on an ordered module to edit only its supported binding and presentation controls.</p>
  </section>
{/if}

<style>
  .module-inspector {
    gap: 14px;
    border: 1px solid var(--line-soft);
  }

  .module-inspector h2 {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .inspector-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .inspector-field label,
  .inspector-check-field label {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .inspector-field small,
  .inspector-check-field small,
  .module-inspector-empty p {
    color: var(--text-dim);
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1.45;
    text-transform: none;
  }

  .inspector-unit {
    color: var(--text-dim);
    font-size: 9px;
    letter-spacing: 0.02em;
    text-transform: none;
  }

  .inspector-range {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 48px;
    gap: 8px;
    align-items: center;
  }

  .inspector-range output {
    padding: 6px 7px;
    color: var(--text-primary);
    background: var(--bg-base);
    border: 1px solid var(--line-soft);
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    font-size: 10px;
    text-align: center;
  }

  .inspector-check-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px;
    background: var(--bg-base);
    border: 1px solid var(--line-soft);
    border-radius: var(--radius-sm);
  }

  .inspector-check-field label {
    justify-content: flex-start;
    align-items: center;
    color: var(--text-primary);
    cursor: pointer;
  }

  .inspector-check-field input {
    accent-color: var(--accent);
  }

  .inspector-diagnostics {
    display: flex;
    flex-direction: column;
    gap: 7px;
  }

  .inspector-diagnostic {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 9px 10px;
    background: color-mix(in srgb, var(--amber) 8%, var(--bg-base));
    border-radius: var(--radius-sm);
    font-size: 10.5px;
    line-height: 1.4;
  }

  .inspector-diagnostic strong {
    color: var(--text-primary);
  }

  .inspector-diagnostic span {
    color: var(--text-muted);
  }

  .inspector-diagnostic small {
    color: var(--text-dim);
  }

  .inspector-diagnostic.diagnostic-error {
    background: color-mix(in srgb, var(--red) 10%, var(--bg-base));
  }

  .inspector-diagnostic.diagnostic-error strong {
    color: var(--red);
  }

  .module-inspector-empty {
    align-items: flex-start;
  }

  .module-inspector-empty h2 {
    margin: 0;
    color: var(--text-primary);
    font-size: 13px;
  }

  .module-inspector-empty p:last-child {
    margin: 0;
  }
</style>
