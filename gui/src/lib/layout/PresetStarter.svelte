<script lang="ts">
  import type { LayoutPreset } from "./types";

  type Props = {
    presets: LayoutPreset[];
    oncreate: (preset: LayoutPreset, name: string) => void;
    busy?: boolean;
  };

  let { presets, oncreate, busy = false }: Props = $props();
  let draftName = $state("my-neon-layout");

  function createFromPreset(preset: LayoutPreset) {
    oncreate(preset, draftName.trim() || preset.id);
  }
</script>

<section class="composer-section preset-starter" aria-labelledby="preset-starter-title">
  <div class="composer-section-heading">
    <div>
      <p class="eyebrow">Start with a recipe</p>
      <h2 id="preset-starter-title">Choose a preset</h2>
    </div>
    <span class="composer-count">{presets.length} available</span>
  </div>

  <label class="composer-field" for="composer-draft-name">
    <span>Name your layout</span>
    <input
      id="composer-draft-name"
      type="text"
      bind:value={draftName}
      autocomplete="off"
      spellcheck="false"
      placeholder="my-neon-layout"
      disabled={busy}
    />
    <small>Saved as a typed layout document. You can change the module order without editing source.</small>
  </label>

  <div class="preset-list">
    {#each presets as preset (preset.id)}
      <article class="preset-card">
        <div class="preset-card-copy">
          <div class="preset-card-title">
            <span class="preset-mark">◆</span>
            <strong>{preset.label}</strong>
          </div>
          <p>{preset.description}</p>
        </div>
        <button
          type="button"
          class="btn-apply preset-use"
          onclick={() => createFromPreset(preset)}
          disabled={busy}
          aria-label={`Create ${draftName.trim() || preset.id} from ${preset.label}`}
        >
          {busy ? "Loading…" : "Use preset"}
        </button>
      </article>
    {/each}
  </div>
</section>
