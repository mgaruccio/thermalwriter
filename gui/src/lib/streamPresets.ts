// Declarative registry of Xvfb streaming presets.
//
// Each entry describes one preset: how to launch it, what per-preset fields
// the user can configure, and which binaries must be resolved before the
// Start button is enabled.
//
// argv is an array of template strings. Fields are substituted at launch time:
//   {config_path}  — resolved absolute path to the seeded wrapper config
//   {custom_path}  — user-supplied file path (custom preset only)
//   {executable}   — resolved absolute binary path
//
// Terminal wrapping is added by StreamTab when needs_terminal=true: the
// resolved terminal binary is prepended with its flag syntax before argv[0].

export type FieldDef =
  | { kind: "config_path"; label: string }
  | { kind: "custom_path"; label: string; filter: string }
  | { kind: "fps"; label: string; min: number; max: number; default: number };

export type StreamPreset = {
  /** Stable identifier used as React key / invoke arg. */
  id: string;
  /** Human-readable name shown in the dropdown. */
  label: string;
  /** Primary binary to resolve (via resolve_binaries). */
  binary: string;
  /** If true, launch inside a terminal emulator (btop, nvtop). */
  needs_terminal: boolean;
  /** argv template. {executable} is replaced with the resolved binary path. */
  argv: string[];
  /** Additional per-preset configuration fields rendered in the StreamTab. */
  fields: FieldDef[];
  /** Default capture FPS for this preset. */
  default_fps: number;
};

// Terminal emulators to probe, in preference order.
// resolve_binaries returns the first one that is present.
export const TERMINAL_BINARIES = ["alacritty", "kitty", "xterm"] as const;
export type TerminalBinary = (typeof TERMINAL_BINARIES)[number];

/**
 * Build the terminal-wrap prefix for a given terminal binary.
 * Returns an argv prefix to prepend before the wrapped command.
 *
 * alacritty: alacritty -e <cmd> [args...]
 * kitty:     kitty -o allow_remote_control=no <cmd> [args...]
 * xterm:     xterm -fa 'IBM Plex Mono' -fs 14 -e <cmd> [args...]
 */
export function terminalArgvPrefix(terminal: string): string[] {
  if (terminal.endsWith("alacritty")) return [terminal, "-e"];
  if (terminal.endsWith("kitty")) return [terminal, "-o", "allow_remote_control=no", "-e"];
  // xterm or unknown — use xterm flag syntax
  return [terminal, "-fa", "IBM Plex Mono", "-fs", "14", "-e"];
}

export const STREAM_PRESETS: StreamPreset[] = [
  {
    id: "conky",
    label: "Conky",
    binary: "conky",
    needs_terminal: false,
    // {executable} is the resolved conky path; {config_path} is substituted
    // from the preset's config_path field value.
    argv: ["{executable}", "-c", "{config_path}"],
    fields: [
      {
        kind: "config_path",
        label: "Config",
      },
    ],
    default_fps: 2,
  },
  {
    id: "cava",
    label: "Cava",
    binary: "cava",
    needs_terminal: false,
    argv: ["{executable}", "-p", "{config_path}"],
    fields: [
      {
        kind: "config_path",
        label: "Config",
      },
    ],
    default_fps: 30,
  },
  {
    id: "btop",
    label: "btop",
    binary: "btop",
    needs_terminal: true,
    // btop renders full-screen TUI; must run inside a terminal emulator.
    argv: ["{executable}"],
    fields: [],
    default_fps: 4,
  },
  {
    id: "nvtop",
    label: "nvtop",
    binary: "nvtop",
    needs_terminal: true,
    argv: ["{executable}"],
    fields: [],
    default_fps: 4,
  },
  {
    id: "custom",
    label: "Custom…",
    binary: "",
    needs_terminal: false,
    // Custom: user supplies a full executable path.
    argv: ["{custom_path}"],
    fields: [
      {
        kind: "custom_path",
        label: "Executable",
        filter: "*",
      },
    ],
    default_fps: 15,
  },
];

/**
 * All binaries that must be resolved for streaming to be available.
 * Includes the primary binaries for all non-custom presets plus all
 * supported terminal emulators (needed for terminal-wrapped presets).
 */
export function allBinariesToResolve(): string[] {
  const bins = STREAM_PRESETS.filter((p) => p.binary !== "").map((p) => p.binary);
  return [...bins, ...TERMINAL_BINARIES];
}

/**
 * Substitute template slots in an argv array.
 *
 * @param argv     The template argv from a StreamPreset.
 * @param resolved Map of binary name → absolute path from resolve_binaries.
 * @param preset   The preset being launched.
 * @param fields   Current field values keyed by field kind.
 * @param terminal Resolved absolute path to the terminal emulator (if needed).
 */
export function buildArgv(
  preset: StreamPreset,
  resolved: Record<string, string | null>,
  fieldValues: Record<string, string>,
  terminal: string | null,
): string[] | null {
  const execPath = resolved[preset.binary] ?? null;
  if (preset.binary !== "" && !execPath) return null; // binary not found

  // Guard: custom preset requires a non-empty path before we can build argv.
  if (preset.id === "custom" && !fieldValues["custom_path"]) return null;

  let argv = preset.argv.map((tok) => {
    if (tok === "{executable}") return execPath ?? tok;
    if (tok === "{config_path}") return fieldValues["config_path"] ?? tok;
    if (tok === "{custom_path}") return fieldValues["custom_path"] ?? tok;
    return tok;
  });

  if (preset.needs_terminal) {
    if (!terminal) return null; // no terminal available
    argv = [...terminalArgvPrefix(terminal), ...argv];
  }

  return argv;
}
