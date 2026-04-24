<script lang="ts">
  // Svelte 5 runes — this scaffold uses $state and $derived intentionally so
  // the rest of the GUI can assume the runes runtime. Task 13 builds on this.
  import { invoke } from "@tauri-apps/api/core";

  let name = $state("Mike");
  let greeting = $state("");
  let error = $state("");

  async function greet() {
    error = "";
    try {
      greeting = await invoke<string>("greet", { name });
    } catch (e) {
      error = String(e);
    }
  }
</script>

<main>
  <h1>Thermalwriter</h1>
  <p>Configuration GUI scaffold.</p>

  <section class="greet">
    <label>
      Your name:
      <input type="text" bind:value={name} />
    </label>
    <button type="button" onclick={greet}>Greet</button>
  </section>

  {#if greeting}
    <p class="result">{greeting}</p>
  {/if}
  {#if error}
    <p class="error">Error: {error}</p>
  {/if}
</main>

<style>
  main {
    max-width: 42rem;
    margin: 0 auto;
    padding: 2rem;
  }
  h1 {
    font-size: 1.6rem;
    margin-bottom: 0.25rem;
  }
  .greet {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    margin-top: 1.25rem;
  }
  .result {
    margin-top: 1rem;
    color: #8ef5c0;
  }
  .error {
    margin-top: 1rem;
    color: #ff8080;
  }
</style>
