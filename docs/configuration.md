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

Selector for the LCD. The default, `"auto"` (case-insensitive), succeeds only
when discovery finds exactly one supported physical device. Set `"all"` to open
every supported display in deterministic order and mirror the same layout to
each (including duplicate `VID:PID` units). When connected devices have
distinct IDs and you want a single display, use a hexadecimal `"VID:PID"`
selector with optional `0x` prefixes, for example `"87ad:70db"` or
`"0x0416:0x5408"`. Two devices sharing the same `VID:PID` cannot be targeted
individually except via `"all"`. Unknown, absent, and ambiguous selections fail
explicitly rather than choosing an arbitrary device. Per-screen independent
configuration is not available in this release.

Mirror membership is refreshed on daemon startup and after a group reconnect; hot-plugging an additional display while another remains active requires restarting the daemon. A fatal output disconnect briefly resets the group before reconnecting the displays still present.
