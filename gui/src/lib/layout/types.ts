export type ModuleKind = "metric" | "sparkline" | "text" | "media";

export type MetricDocument = {
  kind: "metric";
  id: string;
  binding: string;
  variant: string;
};

export type SparklineDocument = {
  kind: "sparkline";
  id: string;
  binding: string;
  variant: string;
};

export type TextDocument = {
  kind: "text";
  id: string;
  binding: string;
  variant: string;
};

export type MediaDocument = {
  kind: "media";
  id: string;
  binding: string;
  variant: string;
  source?: string;
  fit?: "contain" | "cover";
  span_bridge?: boolean;
  opacity?: number;
};

export type LayoutModule =
  | MetricDocument
  | SparklineDocument
  | TextDocument
  | MediaDocument;

export type ProfileRecipeDocument = {
  recipe: string;
  bridge?: string;
};

export type LayoutDocument = {
  version: number;
  name: string;
  preset?: string;
  modules: LayoutModule[];
  profiles: Record<string, ProfileRecipeDocument>;
};

export type LayoutDocumentResponse = {
  document: LayoutDocument;
  document_fingerprint: string;
};

export type LayoutSaveResponse = {
  name: string;
  path: string;
  document_fingerprint: string;
};

export type LayoutActivationState =
  | { state: "active" }
  | { state: "daemon-unavailable"; reason: string }
  | { state: "activation-failed"; reason: string }
  | { state: "active-but-default-not-persisted"; reason: string };

export type LayoutApplyResponse = {
  saved: LayoutSaveResponse;
  activation: LayoutActivationState;
};

export type LayoutDiagnostic = {
  code: string;
  severity: "error" | "warning" | "info";
  message: string;
  file?: string | null;
  line?: number | null;
  column?: number | null;
  profile?: string | null;
  module_id?: string | null;
  property_path?: string | null;
  reason: string;
  fix: string;
};

export type LayoutPreviewResponse = {
  width: number;
  height: number;
  rgba: number[];
  diagnostics: LayoutDiagnostic[];
  topology: "rectangular" | "curved-panorama";
  document_fingerprint: string;
};

export type LayoutPreset = {
  id: string;
  label: string;
  description: string;
};

export type ModuleReorderDirection = "up" | "down";

export type ComposerSaveState = "unsaved" | "saved" | "active";

export const COMPOSER_PRESETS: LayoutPreset[] = [
  {
    id: "neon-composer",
    label: "Neon Composer",
    description: "The flagship CPU metric and history composition, ready to make your own.",
  },
];

export function moduleKindLabel(kind: ModuleKind): string {
  switch (kind) {
    case "metric":
      return "Metric";
    case "sparkline":
      return "Sparkline";
    case "text":
      return "Text";
    case "media":
      return "Media";
  }
}

export function moduleKindDescription(kind: ModuleKind): string {
  switch (kind) {
    case "metric":
      return "A live value or status card";
    case "sparkline":
      return "A compact value history";
    case "text":
      return "A label or status message";
    case "media":
      return "An image-backed visual module";
  }
}

export function moduleBinding(module: LayoutModule): string {
  return module.kind === "media" && module.source ? module.source : module.binding;
}

export function createModule(kind: ModuleKind, existing: LayoutModule[]): LayoutModule {
  const prefix = kind;
  let ordinal = existing.length + 1;
  let id = `${prefix}-${ordinal}`;
  while (existing.some((module) => module.id === id)) {
    ordinal += 1;
    id = `${prefix}-${ordinal}`;
  }

  switch (kind) {
    case "metric":
      return { kind, id, binding: "cpu.temperature", variant: "default" };
    case "sparkline":
      return { kind, id, binding: "cpu.temperature.history", variant: "default" };
    case "text":
      return { kind, id, binding: "cpu.temperature", variant: "body" };
    case "media":
      return {
        kind,
        id,
        binding: "",
        variant: "default",
        source: "",
        fit: "contain",
        span_bridge: false,
        opacity: 1,
      };
  }
}

export function normalizeLayoutName(value: string): string {
  return value.trim().replace(/\.layout\.toml$/i, "");
}
