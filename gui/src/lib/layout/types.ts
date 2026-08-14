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

export type PreviewTopology = "rectangular" | "curved-panorama";

export type PreviewProfileId = "square" | "portrait" | "wide" | "curved";

export type PreviewProfile = {
  id: PreviewProfileId;
  label: string;
  description: string;
  width: number;
  height: number;
  backendProfile: "rectangular" | "thermalright-curved-2400x1080";
  topology: PreviewTopology;
};

export const PREVIEW_PROFILES: readonly PreviewProfile[] = [
  {
    id: "square",
    label: "Square",
    description: "Compact 480 × 480 rectangular surface.",
    width: 480,
    height: 480,
    backendProfile: "rectangular",
    topology: "rectangular",
  },
  {
    id: "portrait",
    label: "Portrait",
    description: "Tall 480 × 1280 rectangular surface.",
    width: 480,
    height: 1280,
    backendProfile: "rectangular",
    topology: "rectangular",
  },
  {
    id: "wide",
    label: "Wide",
    description: "Landscape 1280 × 480 rectangular surface.",
    width: 1280,
    height: 480,
    backendProfile: "rectangular",
    topology: "rectangular",
  },
  {
    id: "curved",
    label: "Curved",
    description: "Thermalright 2400 × 1080 panorama with a protected bridge.",
    width: 2400,
    height: 1080,
    backendProfile: "thermalright-curved-2400x1080",
    topology: "curved-panorama",
  },
];

export type PreviewSurfaceRegion = {
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
};

export type LayoutPreviewResponse = {
  width: number;
  height: number;
  rgba: number[];
  diagnostics: LayoutDiagnostic[];
  topology: PreviewTopology;
  readable_zones?: PreviewSurfaceRegion[];
  protected_regions?: PreviewSurfaceRegion[];
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

export type SensorDescriptor = {
  key: string;
  name: string;
  unit: string;
  cost_us: number;
};

export type BindingKind = "sensor" | "history" | "media";

export type ModuleOption = {
  value: string;
  label: string;
  help: string;
};

export type MediaFitOption = ModuleOption & {
  value: "contain" | "cover";
};

export type ModuleCapability = {
  bindingKind: BindingKind;
  bindingLabel: string;
  bindingHelp: string;
  variants: readonly ModuleOption[];
  fitOptions?: readonly MediaFitOption[];
  opacity?: {
    min: number;
    max: number;
    step: number;
    unit: string;
    help: string;
  };
  bridgeProfiles?: readonly string[];
  bridgeHelp?: string;
};

/**
 * The inspector vocabulary mirrors fields the typed document and renderer
 * already understand. It intentionally has no escape hatch for raw styles.
 */
export const moduleCapabilities: Readonly<Record<ModuleKind, ModuleCapability>> = {
  metric: {
    bindingKind: "sensor",
    bindingLabel: "Sensor binding",
    bindingHelp: "Choose a live sensor from the daemon catalog. The preview uses the selected key.",
    variants: [
      { value: "default", label: "Default", help: "Balanced value card for everyday readings." },
      { value: "hero", label: "Hero", help: "A larger value treatment for the primary reading." },
      { value: "compact", label: "Compact", help: "A tighter value treatment for dense compositions." },
      { value: "status", label: "Status", help: "A status-sized value treatment for state-oriented cards." },
    ],
  },
  sparkline: {
    bindingKind: "history",
    bindingLabel: "History binding",
    bindingHelp: "Choose a sensor history. History keys are derived from the live sensor catalog.",
    variants: [
      { value: "default", label: "Default", help: "The standard accent line." },
      { value: "line", label: "Line", help: "A line-only history treatment." },
      { value: "area", label: "Area", help: "A bounded filled history treatment." },
      { value: "neon", label: "Neon", help: "A brighter, heavier filled history treatment." },
      { value: "muted", label: "Muted", help: "A lower-emphasis history line." },
    ],
  },
  text: {
    bindingKind: "sensor",
    bindingLabel: "Text binding",
    bindingHelp: "Choose a sensor value to render as bounded text.",
    variants: [
      { value: "body", label: "Body", help: "Readable body text." },
      { value: "title", label: "Title", help: "Large title text." },
      { value: "label", label: "Label", help: "A compact label role." },
      { value: "caption", label: "Caption", help: "Lower-emphasis caption text." },
      { value: "value", label: "Value", help: "A value-oriented text role." },
      { value: "unit", label: "Unit", help: "A unit-oriented text role." },
      { value: "status", label: "Status", help: "An accent status role." },
    ],
  },
  media: {
    bindingKind: "media",
    bindingLabel: "Media source",
    bindingHelp: "Use a relative filename below the approved media directory.",
    variants: [],
    fitOptions: [
      { value: "contain", label: "Contain", help: "Keep the full image visible inside the module bounds." },
      { value: "cover", label: "Cover", help: "Fill the module bounds and crop overflow." },
    ],
    opacity: {
      min: 0.7,
      max: 1,
      step: 0.01,
      unit: "0–1",
      help: "Keep media opacity at or above the LCD readability floor.",
    },
    bridgeProfiles: ["thermalright-curved-2400x1080"],
    bridgeHelp: "Available only when a selected curved profile permits media-only bridge spanning.",
  },
};

export function moduleCapabilitiesFor(kind: ModuleKind): ModuleCapability {
  return moduleCapabilities[kind];
}

export function moduleBindingKind(kind: ModuleKind): BindingKind {
  return moduleCapabilitiesFor(kind).bindingKind;
}

/** Return picker entries for the selected module's supported binding kind. */
export function moduleBindingOptions(kind: ModuleKind, sensors: SensorDescriptor[]): SensorDescriptor[] {
  if (moduleBindingKind(kind) !== "history") {
    return [...new Map(sensors.map((sensor) => [sensor.key, sensor])).values()];
  }

  const options = new Map<string, SensorDescriptor>();
  for (const sensor of sensors) {
    if (sensor.key.endsWith(".history")) {
      options.set(sensor.key, sensor);
    }
  }
  for (const sensor of sensors) {
    if (!sensor.key.endsWith(".history")) {
      options.set(`${sensor.key}.history`, {
        ...sensor,
        key: `${sensor.key}.history`,
        name: `${sensor.name} history`,
      });
    }
  }
  return [...options.values()];
}

const LEGACY_SENSOR_KEYS: Record<string, string> = {
  "cpu.temperature": "cpu_temp",
  "cpu.utilization": "cpu_util",
  "cpu.power": "cpu_power",
  "cpu.fan": "cpu_fan",
  "gpu.temperature": "gpu_temp",
  "gpu.utilization": "gpu_util",
  "gpu.power": "gpu_power",
  "gpu.memory.used": "vram_used",
  "gpu.memory.total": "vram_total",
  "memory.used": "ram_used",
  "memory.total": "ram_total",
  "network.receive": "net_rx",
  "network.transmit": "net_tx",
  "game.fps": "fps",
  "game.frametime": "frametime",
};

export function isKnownModuleBinding(
  kind: ModuleKind,
  binding: string,
  sensors: SensorDescriptor[],
) : boolean {
  const value = binding.trim();
  if (!value) return false;
  if (moduleBindingOptions(kind, sensors).some((sensor) => sensor.key === value)) {
    return true;
  }
  const base = value.replace(/\.history$/, "").replace(/_history$/, "");
  const legacy = LEGACY_SENSOR_KEYS[base];
  return Boolean(
    legacy &&
      sensors.some(
        (sensor) => sensor.key === legacy || sensor.key === `${legacy}.history` || sensor.key === base,
      ),
  );
}

export function hasRelevantBridgeProfile(
  profiles: Record<string, ProfileRecipeDocument> | undefined,
 ): boolean {
  return Boolean(profiles && Object.values(profiles).some((profile) => profile.bridge === "media-only"));
}
