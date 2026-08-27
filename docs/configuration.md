# Configuration

The `thermalwriter` daemon is configured via a TOML file located at `~/.config/thermalwriter/config.toml`. If the file does not exist, defaults are used.

## Configuration Schema

Here is the documentation for all configuration blocks and fields.

### `[display]`

Settings related to the cooler LCD rendering and refresh options.

- **`tick_rate`** (Integer)
  - Description: The refresh rate (frames per second) to send to the display.
  - Default: `2`
  - Range: `1..=60`

- **`default_layout`** (String)
  - Description: Layout filename to load on startup, relative to the layouts directory.
  - Default: `"svg/neon-dash-v2.svg"`
  - New composer documents use a `.layout.toml` filename such as `neon-composer.layout.toml`; see the [layout engine guide](layout-engine.md) and [`.layout.toml` authoring reference](../skills/designing-layouts/references/layout-toml.md).

- **`jpeg_quality`** (Integer)
  - Description: Quality level for compression of the JPEG frames sent to the LCD.
  - Default: `85`
  - Range: `10..=100`

- **`rotation`** (Integer)
  - Description: Rotate frames before sending to the device, in degrees.
  - Default: `180`
  - Allowed values: `0`, `90`, `180`, `270`

- **`mode`** (String)
  - Description: Rendering mode.
  - Default: `"svg"`
  - Allowed values: `"svg"`, `"html"`, `"xvfb"`

---

### `[sensors]`

Settings related to polling frequency and custom log path detection.

- **`poll_interval_ms`** (Integer)
  - Description: How often to poll metric providers/sensors in milliseconds.
  - Default: `2000`
  - Range: `100..=60000`

- **`mangohud_log_dir`** (String)
  - Description: Directory containing MangoHud log files.
  - Default: `""` (empty string, auto-detects)

- **`llm`** (Table)
  - Description: vLLM/SGLang inference-server status endpoint.
  - Default: all fields use the values below.
  - Example:
    ```toml
    [sensors.llm]
    url = ""                 # auto-probe 127.0.0.1:8000 then :30000
    engine = "auto"          # auto | vllm | sglang
    api_key = ""              # optional Bearer token
    timeout_ms = 250          # 50..=2000
    ```
  - `url` accepts only `http://` URLs; an empty value enables localhost auto-probing.
  - `api_key` may be omitted when `VLLM_API_KEY` or `SGLANG_API_KEY` is set in the environment.
  - `engine` selects the metrics vocabulary, or `"auto"` detects it from the metrics response.
  - `timeout_ms` controls the blocking HTTP read/write timeout.

---

### `[xvfb]`

Xvfb virtual frame buffer capture options (only active when `display.mode = "xvfb"`).

- **`command`** (String)
  - Description: Shell command to execute inside the virtual display environment.
  - Default: `""` (empty string)

- **`tick_rate`** (Integer)
  - Description: Target capture framerate in frames per second.
  - Default: `15`
  - Range: `1..=60`

---

### `[theme]`

Custom palette colors available for layout files.

- **`source`** (String)
  - Description: Theme source name. Usually `"default"` or `"manual"`.
  - Default: `"default"`

- **`manual`** (Table)
  - Description: Set palette overrides when `source = "manual"`. The keys and their defaults (in this exact order):
    - `primary`: `#e94560`
    - `secondary`: `#53d8fb`
    - `accent`: `#20f5d8`
    - `background`: `#08080f`
    - `surface`: `#12121e`
    - `text`: `#e0e0e0`
    - `text_dim`: `#888888`
    - `success`: `#00ff88`
    - `warning`: `#ffaa00`
    - `critical`: `#ff3333`

---

### `[background]`

Configure background images.

- **`image`** (String)
  - Description: Filename of the background image (no path). The file must live under `~/.config/thermalwriter/backgrounds/`. If omitted (or `None`), no background is applied.

---

### `[layout_vars]`

Per-layout variable overrides keyed by layout filename. This table contains custom overrides specified for a layout.
- Format: `[layout_vars."<layout_filename>"]`
- Example:
  ```toml
  [layout_vars."svg/neon-dash-v2.svg"]
  show_gpu = "true"
  temp_unit = "C"
  ```

### `display.device`

Selector for the LCD when `[[displays]]` is empty. The default, `"auto"`
(case-insensitive), succeeds only when discovery finds exactly one supported
physical device. Set `"all"` to open every supported display in deterministic
order and **mirror** the same layout/mode/rotation to each (including duplicate
`VID:PID` units). When connected devices have distinct IDs and you want a
single display, use a hexadecimal `"VID:PID"` selector with optional `0x`
prefixes, for example `"87ad:70db"` or `"0x0416:0x5408"`. Two devices sharing
the same `VID:PID` cannot be targeted individually except via `"all"`. Unknown,
absent, and ambiguous selections fail explicitly rather than choosing an
arbitrary device.

Mirror membership is refreshed on daemon startup and after a group reconnect;
hot-plugging an additional display while another remains active requires
restarting the daemon. A fatal output disconnect briefly resets the group
before reconnecting the displays still present.

### `[[displays]]` (independent multi-display)

When this array is **non-empty**, it owns device selection and runs an
independent pipeline per entry. `display.device` is ignored for connection.
Omitted per-entry fields inherit from `[display]`.

```toml
[display]
tick_rate = 2
jpeg_quality = 85
# defaults for omitted per-output fields; also the D-Bus primary control surface

[[displays]]
device = "87ad:70db"                      # required VID:PID (not auto/all)
default_layout = "svg/neon-dash-v2.svg"   # optional
mode = "svg"                              # optional: svg | html | xvfb
rotation = 180                            # optional

[[displays]]
device = "0416:5302"
default_layout = "svg/arc-gauge.svg"
mode = "svg"
rotation = 0
```

Rules:

- Each `device` must be a concrete `VID:PID`; duplicates are rejected (use
  `display.device = "all"` to mirror identical IDs).
- At most one entry may use `mode = "xvfb"`.
- Primary = first entry. D-Bus `set_layout` / `set_mode` / status scalars
  describe the primary only; `display_count` reports how many outputs are
  active.
- Each output renders at its own oriented size (no cross-output letterbox).
- Shared sensors/theme/background; fatal disconnect still resets the whole
  group then reconnects.

