<script lang="ts">
  import type {
    ComposerSaveState,
    LayoutPreviewResponse,
    PreviewSurfaceRegion,
  } from "./types";

  type Props = {
    preview: LayoutPreviewResponse | null;
    previewing: boolean;
    draftName: string;
    profileLabel?: string;
    saveState: ComposerSaveState;
    nativeDimensionsAvailable: boolean;
    error?: string;
  };

  let {
    preview,
    previewing,
    draftName,
    profileLabel = "Selected profile",
    saveState,
    nativeDimensionsAvailable,
    error = "",
  }: Props = $props();
  let canvas = $state<HTMLCanvasElement>();
  let copyPending = $state(false);
  let copyFeedback = $state("");
  let copyFeedbackKind = $state<"success" | "fallback" | "failure" | "">("");

  function validRegion(region: PreviewSurfaceRegion, frame: LayoutPreviewResponse): boolean {
    return (
      Number.isFinite(region.x) &&
      Number.isFinite(region.y) &&
      Number.isFinite(region.width) &&
      Number.isFinite(region.height) &&
      region.width > 0 &&
      region.height > 0 &&
      region.x >= 0 &&
      region.y >= 0 &&
      region.x + region.width <= frame.width &&
      region.y + region.height <= frame.height
    );
  }

  function topologyRegions(frame: LayoutPreviewResponse) {
    const readableZones = (frame.readable_zones ?? []).filter((region) => validRegion(region, frame));
    const protectedRegions = (frame.protected_regions ?? []).filter((region) => validRegion(region, frame));
    if (readableZones.length >= 2 && protectedRegions.length > 0) {
      return { readableZones, protectedRegions };
    }

    // The backend currently sends the explicit topology enum. Keep the guide
    // conservative and proportional to the registered 40/20/40 surface zones
    // until richer bounds metadata is available in the response.
    return {
      readableZones: [
        { name: "left-readable", x: 0, y: 0, width: frame.width * 0.4, height: frame.height },
        {
          name: "right-readable",
          x: frame.width * 0.6,
          y: 0,
          width: frame.width * 0.4,
          height: frame.height,
        },
      ],
      protectedRegions: [
        {
          name: "center-bridge",
          x: frame.width * 0.4,
          y: 0,
          width: frame.width * 0.2,
          height: frame.height,
        },
      ],
    };
  }

  function drawRegionLabel(
    context: CanvasRenderingContext2D,
    region: PreviewSurfaceRegion,
    label: string,
    fill: string,
    stroke: string,
    frame: LayoutPreviewResponse,
  ) {
    const scale = Math.max(1, Math.min(frame.width, frame.height) / 480);
    const lineWidth = Math.max(2, Math.round(scale * 2));
    const fontSize = Math.max(18, Math.round(scale * 14));
    const padding = Math.max(8, Math.round(scale * 6));
    const x = region.x;
    const y = region.y;
    const width = region.width;
    const height = region.height;
    const textWidth = context.measureText(label).width;
    const labelWidth = Math.min(width - lineWidth * 2, textWidth + padding * 2);
    const labelHeight = fontSize + padding * 1.5;
    const labelX = x + Math.max(lineWidth, (width - labelWidth) / 2);
    const labelY = y + Math.max(lineWidth, (height - labelHeight) / 2);

    context.fillStyle = fill;
    context.fillRect(x, y, width, height);
    context.strokeStyle = stroke;
    context.lineWidth = lineWidth;
    context.setLineDash([lineWidth * 4, lineWidth * 3]);
    context.strokeRect(x, y, width, height);
    context.setLineDash([]);

    context.fillStyle = "rgba(4, 7, 12, 0.84)";
    context.fillRect(labelX, labelY, labelWidth, labelHeight);
    context.strokeStyle = stroke;
    context.lineWidth = Math.max(1, Math.round(scale));
    context.strokeRect(labelX, labelY, labelWidth, labelHeight);
    context.fillStyle = "rgba(245, 248, 255, 0.96)";
    context.textAlign = "center";
    context.textBaseline = "middle";
    context.fillText(label, labelX + labelWidth / 2, labelY + labelHeight / 2);
  }

  function drawTopologyOverlay(context: CanvasRenderingContext2D, frame: LayoutPreviewResponse) {
    if (frame.topology !== "curved-panorama") return;

    const { readableZones, protectedRegions } = topologyRegions(frame);
    const scale = Math.max(1, Math.min(frame.width, frame.height) / 480);
    const fontSize = Math.max(18, Math.round(scale * 14));
    context.save();
    context.font = `600 ${fontSize}px ${"ui-monospace, SFMono-Regular, Menlo, monospace"}`;

    readableZones.forEach((region, index) => {
      drawRegionLabel(
        context,
        region,
        index === 0 ? "LEFT READABLE" : "RIGHT READABLE",
        "rgba(45, 212, 191, 0.12)",
        "rgba(45, 212, 191, 0.92)",
        frame,
      );
    });
    protectedRegions.forEach((region) => {
      drawRegionLabel(
        context,
        region,
        "PROTECTED BRIDGE",
        "rgba(251, 191, 36, 0.2)",
        "rgba(251, 191, 36, 0.98)",
        frame,
      );
    });
    context.restore();
  }

  function clearCopyFeedback() {
    copyFeedback = "";
    copyFeedbackKind = "";
  }

  function previewIdentity(frame: LayoutPreviewResponse) {
    const layout = draftName.trim() || "Untitled layout";
    const profile = profileLabel.trim() || "Selected profile";
    return `${layout} · ${profile} · ${frame.width} × ${frame.height} native pixels`;
  }

  function previewFileName(frame: LayoutPreviewResponse) {
    const slug = (value: string) =>
      value.trim().replace(/[^a-z0-9]+/gi, "-").replace(/^-+|-+$/g, "").toLowerCase();
    const layout = slug(draftName) || "layout";
    const profile = slug(profileLabel) || "preview";
    return `${layout}-${profile}-${frame.width}x${frame.height}.png`;
  }

  function visiblePreviewToPngBlob(): Promise<Blob> {
    const target = canvas;
    const frame = preview;
    if (!target || !frame || frame.rgba.length === 0) {
      return Promise.reject(new Error("No visible preview is ready."));
    }
    if (frame.rgba.length !== frame.width * frame.height * 4) {
      return Promise.reject(new Error("The visible preview is still rendering."));
    }
    if (target.width !== frame.width || target.height !== frame.height) {
      return Promise.reject(new Error("The visible preview is still rendering."));
    }

    return new Promise((resolve, reject) => {
      target.toBlob((blob) => {
        if (blob) {
          resolve(blob);
        } else {
          reject(new Error("The visible preview could not be encoded as PNG."));
        }
      }, "image/png");
    });
  }

  function savePng(blob: Blob, frame: LayoutPreviewResponse) {
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    const filename = previewFileName(frame);
    link.href = url;
    link.download = filename;
    link.style.display = "none";
    document.body.appendChild(link);
    link.click();
    link.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 0);
    return filename;
  }

  function canWriteImageToClipboard() {
    return typeof navigator !== "undefined" && typeof navigator.clipboard?.write === "function" && typeof ClipboardItem !== "undefined";
  }

  async function copyVisiblePreview() {
    if (copyPending || !preview || preview.rgba.length === 0) return;

    copyPending = true;
    clearCopyFeedback();
    try {
      const frame = preview;
      const blob = await visiblePreviewToPngBlob();
      if (preview !== frame) {
        throw new Error("The selected profile changed during capture. Try again.");
      }

      if (canWriteImageToClipboard()) {
        try {
          await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
          copyFeedback = `Copied ${previewIdentity(frame)} as PNG.`;
          copyFeedbackKind = "success";
          return;
        } catch {
          // A denied or unavailable image clipboard uses the explicit PNG fallback below.
        }
      }

      const filename = savePng(blob, frame);
      copyFeedback = `Image clipboard unavailable; saved ${previewIdentity(frame)} as ${filename}.`;
      copyFeedbackKind = "fallback";
    } catch (captureError) {
      const detail = captureError instanceof Error ? captureError.message : "Try again.";
      copyFeedback = `Could not copy ${preview ? previewIdentity(preview) : "the visible preview"}. ${detail}`;
      copyFeedbackKind = "failure";
    } finally {
      copyPending = false;
    }
  }

  async function saveVisiblePreview() {
    if (copyPending || !preview || preview.rgba.length === 0) return;

    copyPending = true;
    clearCopyFeedback();
    try {
      const frame = preview;
      const blob = await visiblePreviewToPngBlob();
      if (preview !== frame) {
        throw new Error("The selected profile changed during capture. Try again.");
      }
      const filename = savePng(blob, frame);
      copyFeedback = `Saved ${previewIdentity(frame)} as ${filename}.`;
      copyFeedbackKind = "success";
    } catch (captureError) {
      const detail = captureError instanceof Error ? captureError.message : "Try again.";
      copyFeedback = `Could not save ${preview ? previewIdentity(preview) : "the visible preview"}. ${detail}`;
      copyFeedbackKind = "failure";
    } finally {
      copyPending = false;
    }
  }

  $effect(() => {
    const frame = preview;
    const target = canvas;
    clearCopyFeedback();
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
    drawTopologyOverlay(context, frame);
  });

  const diagnosticSummary = $derived(preview?.diagnostics ?? []);
  const hasFrame = $derived(Boolean(preview && preview.rgba.length === preview.width * preview.height * 4));
  const hasCurvedTopology = $derived(preview?.topology === "curved-panorama");
</script>

<div class="typed-preview" aria-live="polite">
  <div class="typed-preview-frame">
    {#if hasFrame && preview}
      <canvas
        bind:this={canvas}
        width={preview.width}
        height={preview.height}
        data-native-width={preview.width}
        data-native-height={preview.height}
        data-topology={preview.topology}
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
            : "Choose a supported native profile to preview the display surface."}
        </span>
      </div>
    {/if}
  </div>

  {#if hasCurvedTopology}
    <div class="typed-preview-topology-legend" role="note" aria-label="Curved panorama topology guide">
      <span class="topology-legend-item topology-readable"><i aria-hidden="true"></i>Left / right readable zones</span>
      <span class="topology-legend-item topology-bridge"><i aria-hidden="true"></i>Protected bridge</span>
      <small>Illustrative topology guide · no calibrated optical warp</small>
    </div>
  {/if}

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

  {#if hasFrame && preview}
    <div class="typed-preview-actions" aria-label="Preview handoff actions">
      <button
        type="button"
        class="btn-secondary"
        onclick={copyVisiblePreview}
        disabled={copyPending}
        aria-describedby="preview-handoff-status"
      >
        {copyPending ? "Preparing PNG…" : "Copy preview image"}
      </button>
      <button
        type="button"
        class="btn-secondary"
        onclick={saveVisiblePreview}
        disabled={copyPending}
        aria-describedby="preview-handoff-status"
      >
        Save PNG
      </button>
    </div>
  {/if}

  {#if copyFeedback}
    <p
      id="preview-handoff-status"
      class:failure={copyFeedbackKind === "failure"}
      class:fallback={copyFeedbackKind === "fallback"}
      class="typed-preview-feedback"
      role={copyFeedbackKind === "failure" ? "alert" : "status"}
      aria-live="polite"
    >
      {copyFeedback}
    </p>
  {/if}

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

<style>
  .typed-preview-topology-legend {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
    padding: 0 4px;
    color: var(--text-dim);
    font-family: var(--font-mono);
    font-size: 9px;
    line-height: 1.35;
  }

  .topology-legend-item {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    white-space: nowrap;
  }

  .topology-legend-item i {
    display: inline-block;
    width: 8px;
    height: 8px;
    border: 1px solid currentColor;
    border-radius: 2px;
  }

  .topology-readable {
    color: var(--teal, #2dd4bf);
  }

  .topology-readable i {
    background: color-mix(in srgb, currentColor 18%, transparent);
  }

  .topology-bridge {
    color: var(--amber);
  }

  .topology-bridge i {
    background: color-mix(in srgb, currentColor 20%, transparent);
  }

  .typed-preview-topology-legend small {
    flex-basis: 100%;
    color: var(--text-dim);
  }

  .typed-preview-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .typed-preview-feedback {
    margin: 0;
    padding: 8px 10px;
    color: var(--green);
    background: color-mix(in srgb, var(--green) 9%, var(--bg-elev));
    border: 1px solid color-mix(in srgb, var(--green) 24%, var(--line-soft));
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    font-size: 9.5px;
    line-height: 1.4;
  }

  .typed-preview-feedback.fallback {
    color: var(--amber);
    background: color-mix(in srgb, var(--amber) 9%, var(--bg-elev));
    border-color: color-mix(in srgb, var(--amber) 24%, var(--line-soft));
  }

  .typed-preview-feedback.failure {
    color: var(--red);
    background: color-mix(in srgb, var(--red) 10%, var(--bg-elev));
    border-color: color-mix(in srgb, var(--red) 24%, var(--line-soft));
  }
</style>
