<script lang="ts">
  import { formatErrorForClipboard } from "./formatErrorForClipboard";
  import type { LayoutDiagnostic } from "./types";

  type Props = {
    diagnostics: LayoutDiagnostic[];
  };

  let { diagnostics }: Props = $props();
  let selectedIndex = $state(0);
  let copyPending = $state(false);
  let feedback = $state("");
  let feedbackKind = $state<"success" | "failure" | "">("");

  const selectedDiagnostic = $derived(diagnostics[selectedIndex] ?? null);

  $effect(() => {
    if (selectedIndex >= diagnostics.length) {
      selectedIndex = Math.max(0, diagnostics.length - 1);
    }
  });

  function clearFeedback() {
    feedback = "";
    feedbackKind = "";
  }

  async function copySelectedError() {
    if (!selectedDiagnostic || copyPending) return;

    copyPending = true;
    clearFeedback();
    try {
      if (!navigator.clipboard?.writeText) {
        throw new Error("Clipboard API unavailable");
      }
      await navigator.clipboard.writeText(formatErrorForClipboard(selectedDiagnostic));
      feedback = "Error copied to the clipboard.";
      feedbackKind = "success";
    } catch {
      feedback = "Could not copy the error. Select the text and copy it manually.";
      feedbackKind = "failure";
    } finally {
      copyPending = false;
    }
  }
</script>

{#if diagnostics.length > 0}
  <section class="composer-section validation-panel" aria-labelledby="validation-panel-title">
    <div class="composer-section-heading">
      <div>
        <p class="eyebrow">Validation</p>
        <h2 id="validation-panel-title">Layout diagnostics</h2>
      </div>
      <span class="composer-count">{diagnostics.length} {diagnostics.length === 1 ? "issue" : "issues"}</span>
    </div>

    <label class="validation-picker" for="validation-diagnostic-select">
      <span>Diagnostic to copy</span>
      <select id="validation-diagnostic-select" bind:value={selectedIndex} onchange={clearFeedback}>
        {#each diagnostics as diagnostic, index}
          <option value={index}>{diagnostic.code} · {diagnostic.message}</option>
        {/each}
      </select>
    </label>

    {#if selectedDiagnostic}
      <div class="validation-diagnostic" aria-live="polite">
        <div class="validation-diagnostic-heading">
          <strong>{selectedDiagnostic.code}</strong>
          <span class="diagnostic-severity diagnostic-{selectedDiagnostic.severity}">{selectedDiagnostic.severity}</span>
        </div>
        <p class="validation-message">{selectedDiagnostic.message}</p>
        <dl class="validation-fields">
          <div>
            <dt>Profile</dt>
            <dd>{selectedDiagnostic.profile ?? "-"}</dd>
          </div>
          <div>
            <dt>Module</dt>
            <dd>{selectedDiagnostic.module_id ?? "-"}</dd>
          </div>
          <div>
            <dt>Property</dt>
            <dd>{selectedDiagnostic.property_path ?? "-"}</dd>
          </div>
          <div>
            <dt>Reason</dt>
            <dd>{selectedDiagnostic.reason}</dd>
          </div>
          <div>
            <dt>Fix</dt>
            <dd>{selectedDiagnostic.fix}</dd>
          </div>
        </dl>

        <button type="button" class="btn-secondary validation-copy" onclick={copySelectedError} disabled={copyPending}>
          {copyPending ? "Copying…" : "Copy Error"}
        </button>
      </div>
    {/if}

    {#if feedback}
      <p class:failure={feedbackKind === "failure"} class="validation-feedback" role={feedbackKind === "failure" ? "alert" : "status"}>
        {feedback}
      </p>
    {/if}
  </section>
{/if}

<style>
  .validation-panel {
    gap: 14px;
    border-color: color-mix(in srgb, var(--accent) 28%, var(--line-soft));
  }

  .validation-picker {
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .validation-picker select {
    text-transform: none;
  }

  .validation-diagnostic {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 11px;
    background: var(--bg-base);
    border: 1px solid var(--line-soft);
    border-radius: var(--radius-sm);
  }

  .validation-diagnostic-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }

  .validation-diagnostic-heading strong {
    color: var(--text-primary);
    font-family: var(--font-mono);
    font-size: 11px;
  }

  .diagnostic-severity {
    color: var(--amber);
    font-family: var(--font-mono);
    font-size: 9px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .diagnostic-severity.diagnostic-error {
    color: var(--red);
  }

  .diagnostic-severity.diagnostic-info {
    color: var(--cyan);
  }

  .validation-message {
    margin: 0;
    color: var(--text-primary);
    font-size: 12px;
  }

  .validation-fields {
    display: flex;
    flex-direction: column;
    gap: 5px;
    margin: 0;
    font-size: 10.5px;
    line-height: 1.4;
  }

  .validation-fields div {
    display: grid;
    grid-template-columns: 68px minmax(0, 1fr);
    gap: 8px;
  }

  .validation-fields dt {
    color: var(--text-dim);
    font-family: var(--font-mono);
    text-transform: uppercase;
  }

  .validation-fields dd {
    margin: 0;
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }

  .validation-copy {
    align-self: flex-start;
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--line-soft));
  }

  .validation-feedback {
    margin: 0;
    color: var(--green);
    font-family: var(--font-mono);
    font-size: 10px;
  }

  .validation-feedback.failure {
    color: var(--red);
  }
</style>
