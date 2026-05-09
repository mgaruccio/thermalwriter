<script lang="ts">
  type Props = {
    backgrounds: string[];
    selected: string | null;
    onselect: (name: string | null) => void;
  };

  let { backgrounds, selected, onselect }: Props = $props();
</script>

<div class="bg-gallery">
  <button
    type="button"
    class="bg-tile"
    class:active={selected === null}
    onclick={() => onselect(null)}
  >
    <span class="bg-none-icon">&#x2205;</span>
    <small>None</small>
  </button>

  {#each backgrounds as name}
    <button
      type="button"
      class="bg-tile"
      class:active={selected === name}
      onclick={() => onselect(name)}
      title={name}
    >
      <span class="bg-filename">{name}</span>
    </button>
  {/each}
</div>

<style>
  .bg-gallery {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px 0;
  }

  .bg-tile {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border: 1px solid transparent;
    border-radius: 4px;
    background: transparent;
    color: inherit;
    cursor: pointer;
    text-align: left;
    font-size: 0.85rem;
    transition: background 0.1s;
  }

  .bg-tile:hover {
    background: rgba(255, 255, 255, 0.06);
  }

  .bg-tile.active {
    border-color: var(--accent, #4fc3f7);
    background: rgba(79, 195, 247, 0.1);
  }

  .bg-none-icon {
    font-size: 1rem;
    opacity: 0.6;
  }

  .bg-filename {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
