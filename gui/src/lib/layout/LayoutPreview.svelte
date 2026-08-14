<script lang="ts">
  import type { ComposerSaveState, LayoutPreviewResponse } from "./types";

  type Props = {
    preview: LayoutPreviewResponse | null;
    previewing: boolean;
    draftName: string;
    saveState: ComposerSaveState;
    nativeDimensionsAvailable: boolean;
    error?: string;
  };

  let {
    preview,
    previewing,
    draftName,
    saveState,
    nativeDimensionsAvailable,
    error = "",
  }: Props = $props();
  let canvas = $state<HTMLCanvasElement>();

  $effect(() => {
    const frame = preview;
    const target = canvas;
    if (!frame || !target || frame.rgba.length === 0) return;

    const expectedBytes = frame.width * frame.height * 4;
    if (frame.rgba.length !== expectedBytes) return;

    target.width = frame.width;
    target.height = frame.height;
    const context = target.getContext("2d");
    if (!context) return;
    context.putImageData(
      new ImageData(new Uint8ClampedArray(frame.rgba), frame.width, frame.height),
      0,
      0,
    );
  });

  const diagnosticSummary = $derived(preview?.diagnostics ?? []);
  const hasFrame = $derived(Boolean(preview && preview.rgba.length > 0));
</script>

<div class="typed-preview" aria-live="polite">
  <div class="typed-preview-frame">
    {#if hasFrame && preview}
      <canvas
        bind:this={canvas}
        width={preview.width}
        height={preview.height}
        style={`--preview-width: ${preview.width}; --preview-height: ${preview.height};`}
        aria-label={`${draftName || "Layout"} preview`}
      ></canvas>
    {:else if previewing}
      <div class="typed-preview-empty">
        <span class="preview-pulse" aria-hidden="true"></span>
        <strong>Rendering composition…</strong>
        <span>The native display surface is being checked.</span>
      </div>
    {:else}
      <div class="typed-preview-empty">
        <span class="preview-empty-mark" aria-hidden="true">◈</span>
        <strong>{nativeDimensionsAvailable ? "No preview yet" : "Native dimensions unavailable"}</strong>
        <span>
          {nativeDimensionsAvailable
            ? "Create or reopen a composition to see the display surface."
            : "Connect the daemon to preview at its reported native display size."}
        </span>
      </div>
    {/if}
  </div>

  <div class="typed-preview-meta">
    <div>
      <strong>{draftName || "No composition"}</strong>
      {#if preview}
        <span>{preview.width} × {preview.height} native pixels · {preview.topology}</span>
      {:else}
        <span>Native surface preview</span>
      {/if}
    </div>
    <div class="typed-preview-states">
      <span class="composer-state save-state-{saveState}">{saveState}</span>
      <span class="composer-state preview-state" class:busy={previewing}>
        {previewing ? "previewing" : hasFrame ? "ready" : "waiting"}
      </span>
    </div>
  </div>

  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}
  {#if diagnosticSummary.length > 0}
    <div class="typed-preview-diagnostics" role="status">
      {#each diagnosticSummary as diagnostic (diagnostic.code + (diagnostic.module_id ?? ""))}
        <div class="typed-preview-diagnostic diagnostic-{diagnostic.severity}">
          <strong>{diagnostic.message}</strong>
          <span>{diagnostic.reason}</span>
          <small>{diagnostic.fix}</small>
        </div>
      {/each}
    </div>
  {/if}
</div>
