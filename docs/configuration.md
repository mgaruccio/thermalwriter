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
  - Default: `1000`
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

### `[media]`

Now-playing metadata from MPRIS players on the session D-Bus.

- **`enabled`** (Boolean)
  - Description: Poll MPRIS players for track metadata and expose `track_*` sensor keys.
  - Default: `true`

- **`player`** (String)
  - Description: Optional bus-name substring filter (e.g. `spotify`, `firefox`). Empty string selects any playing player automatically.
  - Default: `""`

- **`album_art_background`** (Boolean)
  - Description: While a track is Playing or Paused and local album art is available, composite that art as the live SVG background without writing `[background].image`. Remote `https://` art URLs are skipped in v1; `file://` and absolute paths work, and `file://` URIs are percent-decoded (e.g. `file:///home/user/Album%20Art.jpg`). HTML/Xvfb modes ignore this override.
  - Default: `false`


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
when discovery finds exactly one supported physical device. When connected
devices have distinct IDs, use a hexadecimal `"VID:PID"` selector with optional
`0x` prefixes, for example `"87ad:70db"` or `"0x0416:0x5408"`. Two devices
sharing the same `VID:PID` remain ambiguous and cannot currently be selected
individually. Unknown, absent, and ambiguous selections fail explicitly rather
than choosing an arbitrary device.
