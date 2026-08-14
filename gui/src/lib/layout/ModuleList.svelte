<script lang="ts">
  import {
    moduleBinding,
    moduleKindDescription,
    moduleKindLabel,
    type LayoutModule,
    type ModuleKind,
    type ModuleReorderDirection,
  } from "./types";

  type Props = {
    modules: LayoutModule[];
    onadd: (kind: ModuleKind) => void;
    onremove: (id: string) => void;
    onreorder: (id: string, direction: ModuleReorderDirection) => void;
    disabled?: boolean;
  };

  let { modules, onadd, onremove, onreorder, disabled = false }: Props = $props();
  const moduleKinds: ModuleKind[] = ["metric", "sparkline", "text", "media"];

  function moveLabel(index: number, direction: ModuleReorderDirection): string {
    return direction === "up"
      ? `Move module ${index + 1} up`
      : `Move module ${index + 1} down`;
  }
</script>

<section class="composer-section module-list-section" aria-labelledby="module-list-title">
  <div class="composer-section-heading">
    <div>
      <p class="eyebrow">Your composition</p>
      <h2 id="module-list-title">Ordered modules</h2>
    </div>
    <span class="composer-count">{modules.length} {modules.length === 1 ? "module" : "modules"}</span>
  </div>

  <div class="module-add-grid" aria-label="Add a module">
    {#each moduleKinds as kind (kind)}
      <button
        type="button"
        class="module-add-button module-kind-{kind}"
        onclick={() => onadd(kind)}
        disabled={disabled}
        title={moduleKindDescription(kind)}
      >
        <span aria-hidden="true">+</span>
        <span>{moduleKindLabel(kind)}</span>
      </button>
    {/each}
  </div>

  {#if modules.length === 0}
    <div class="module-empty">
      <strong>No modules yet</strong>
      <span>Add a typed module above. The order here is the order the layout engine solves.</span>
    </div>
  {:else}
    <ol class="module-list" aria-label="Layout modules">
      {#each modules as module, index (module.id)}
        <li class="module-card module-kind-{module.kind}">
          <div class="module-index" aria-hidden="true">{String(index + 1).padStart(2, "0")}</div>
          <div class="module-card-copy">
            <div class="module-card-title">
              <strong>{moduleKindLabel(module.kind)}</strong>
              <span class="module-type">{module.variant || "default"}</span>
            </div>
            <span class="module-binding">{moduleBinding(module) || "Source to be selected"}</span>
            <span class="module-id">Stable ID · {module.id}</span>
          </div>
          <div class="module-actions">
            <button
              type="button"
              class="icon-button"
              onclick={() => onreorder(module.id, "up")}
              disabled={disabled || index === 0}
              aria-label={moveLabel(index, "up")}
              title="Move up"
            >
              ↑
            </button>
            <button
              type="button"
              class="icon-button"
              onclick={() => onreorder(module.id, "down")}
              disabled={disabled || index === modules.length - 1}
              aria-label={moveLabel(index, "down")}
              title="Move down"
            >
              ↓
            </button>
            <button
              type="button"
              class="icon-button remove"
              onclick={() => onremove(module.id)}
              disabled={disabled}
              aria-label={`Remove module ${index + 1}`}
              title="Remove module"
            >
              ×
            </button>
          </div>
        </li>
      {/each}
    </ol>
  {/if}
</section>
