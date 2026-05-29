<script lang="ts">
  import { onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";

  type Props = {
    backgrounds: string[];
    selected: string | null;
    onselect: (name: string | null) => void;
    onimported: (name: string) => void;
  };

  let { backgrounds, selected, onselect, onimported }: Props = $props();

  let fileInput: HTMLInputElement | undefined = $state();
  let importing = $state(false);
  let importError = $state("");

  async function onFilePicked(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    // Reset so picking the same file again re-fires the change event.
    input.value = "";
    if (!file) return;
    importing = true;
    importError = "";
    try {
      const buffer = await file.arrayBuffer();
      // Vec<u8> on the Rust side; a plain number array round-trips reliably.
      const data = Array.from(new Uint8Array(buffer));
      const stored = await invoke<string>("import_background", {
        filename: file.name,
        data,
      });
      onimported(stored);
    } catch (err) {
      importError = String(err);
    } finally {
      importing = false;
    }
  }

  // Cache filename → object URL so we don't refetch on every selection change.
  // Object URLs need an explicit revoke when the component unmounts.
  const thumbs = $state<Record<string, string>>({});
  const inflight = new Set<string>();

  $effect(() => {
    for (const name of backgrounds) {
      void loadThumb(name);
    }
  });

  onDestroy(() => {
    for (const url of Object.values(thumbs)) {
      URL.revokeObjectURL(url);
    }
  });

  async function loadThumb(name: string) {
    if (thumbs[name] || inflight.has(name)) return;
    inflight.add(name);
    try {
      const buffer = await invoke<ArrayBuffer>("read_background", { name });
      const mime = mimeFor(name);
      const blob = new Blob([buffer], { type: mime });
      thumbs[name] = URL.createObjectURL(blob);
    } catch {
      // Leave entry unset — the placeholder renders.
    } finally {
      inflight.delete(name);
    }
  }

  function mimeFor(name: string): string {
    const lower = name.toLowerCase();
    if (lower.endsWith(".jpg") || lower.endsWith(".jpeg")) return "image/jpeg";
    return "image/png";
  }

  function displayName(name: string): string {
    return name.replace(/\.(png|jpg|jpeg)$/i, "");
  }
</script>

<div class="bg-gallery">
  <button
    type="button"
    class="bg-tile"
    class:active={selected === null}
    onclick={() => onselect(null)}
    title="No background"
  >
    <div class="bg-tile-thumb none">&#x2300;</div>
    <span class="bg-tile-label">None</span>
  </button>

  {#each backgrounds as name}
    <button
      type="button"
      class="bg-tile"
      class:active={selected === name}
      onclick={() => onselect(name)}
      title={name}
    >
      <div class="bg-tile-thumb">
        {#if thumbs[name]}
          <img src={thumbs[name]} alt="" />
        {:else}
          <span>&hellip;</span>
        {/if}
      </div>
      <span class="bg-tile-label">{displayName(name)}</span>
    </button>
  {/each}

  <button
    type="button"
    class="bg-tile import"
    onclick={() => fileInput?.click()}
    disabled={importing}
    title="Import a PNG or JPEG"
  >
    <div class="bg-tile-thumb import">
      <span>{importing ? "…" : "+"}</span>
    </div>
    <span class="bg-tile-label">{importing ? "Importing" : "Import"}</span>
  </button>

  <input
    bind:this={fileInput}
    type="file"
    accept="image/png,image/jpeg"
    style="display: none"
    onchange={onFilePicked}
  />
</div>

{#if importError}
  <p class="bg-import-error">{importError}</p>
{/if}
