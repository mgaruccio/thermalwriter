# Playing Doom 3 on the LCD

The original 2004 **Doom 3** is playable on a Thermalright LCD through
thermalwriter's Xvfb stream mode. This recipe was validated end to end on a
480×480 bulk device with the original Steam game data, dhewm3 1.5.5, Mesa
llvmpipe, and an 8BitDo controller.

This is a deliberately heavyweight demo, not an always-on layout. Software
OpenGL rendering can consume several CPU cores, and the README's layout-mode
footprint measurements do not apply while it is running.

## Requirements

- Original Doom 3 game data, patched to 1.3.1. The Steam release is App ID
  `9050`; **Doom 3: BFG Edition is not supported by dhewm3**.
- [`dhewm3`](https://dhewm3.org/) 1.5.5 or newer.
- Xvfb and thermalwriter's normal mirror-mode dependencies.
- Recommended: an SDL-compatible gamepad.
- Optional: x11vnc and TigerVNC for a keyboard/viewer window.

On Arch-based systems, `dhewm3` is available from the AUR. The other package
names are typically `xorg-server-xvfb`, `x11vnc`, and `tigervnc`.

The default Steam data directory is:

```text
~/.local/share/Steam/steamapps/common/Doom 3
```

A valid installation contains `base/pak000.pk4` through `base/pak008.pk4`.
`fs_basepath` lets dhewm3 use those files in place; do not copy the 1.5 GB game
data into `/usr/share/dhewm3`.

## Install the controller bindings

dhewm3 detects and hot-plugs SDL gamepads, but intentionally supplies no
in-game bindings by default. From a thermalwriter checkout, install the
included bindings into the dedicated save path used below:

```sh
D3_SAVE="${XDG_DATA_HOME:-$HOME/.local/share}/dhewm3-thermalwriter"
install -Dm0644 examples/doom3-gamepad.cfg "$D3_SAVE/base/gamepad.cfg"
```

The launch command executes this file on every run, so the bindings do not
depend on dhewm3 getting a chance to save its configuration during shutdown.
A controller may be turned on before or after the game starts; dhewm3 supports
hot-plugging. Its console/log should report `Detected Gamepad ...` when SDL has
opened it.

Default controls:

| Input | Action |
| --- | --- |
| Left stick | Move |
| Right stick | Look |
| Right trigger | Attack |
| Left trigger | Flashlight |
| South/A button | Jump |
| West/X button | Reload |
| Stick clicks | Sprint / crouch |
| Shoulder buttons | Previous / next weapon |
| D-pad | Weapon shortcuts |
| Start | Open or close the menu |

## Launch at 480×480

Set the Steam data and dedicated save paths, then start dhewm3 as a structured
mirror command:

```sh
D3_DATA="${D3_DATA:-$HOME/.local/share/Steam/steamapps/common/Doom 3}"
D3_SAVE="${XDG_DATA_HOME:-$HOME/.local/share}/dhewm3-thermalwriter"

test -f "$D3_DATA/base/pak008.pk4"
test -f "$D3_SAVE/base/gamepad.cfg"

thermalwriter ctl mirror /usr/bin/dhewm3 \
  +set fs_basepath "$D3_DATA" \
  +set fs_savepath "$D3_SAVE" \
  +set r_fullscreen 0 +set r_mode -1 \
  +set r_customWidth 480 +set r_customHeight 480 \
  +set r_windowX 0 +set r_windowY 0 \
  +set r_multiSamples 0 +set r_shadows 0 \
  +set in_nograb 1 +set in_useGamepad 1 \
  +set com_machineSpec 0 +set com_skipIntroVideos 1 \
  +exec gamepad.cfg +map game/mars_city1
```

Important settings:

- `r_fullscreen 0` is intentional. With no window manager inside Xvfb, SDL's
  fullscreen transition times out and dhewm3 falls back to 640×480. A 480×480
  borderless root window fills the framebuffer correctly.
- `r_shadows 0` avoids severe stencil-shadow corruption in Doom 3's 3D scenes
  when Mesa llvmpipe renders into Xvfb. This is renderer corruption, not JPEG
  or USB artifacting.
- `in_nograb 1` prevents Doom 3's relative mouse recentering from fighting
  x11vnc's absolute pointer events. It does not affect a gamepad.
- `+map game/mars_city1` starts the campaign intro directly. Omit it to start
  at the main menu.

For a non-480×480 LCD, substitute the negotiated dimensions reported by
`thermalwriter ctl status` for `r_customWidth` and `r_customHeight`.

## Set the stream rate

Mirror mode starts at `[xvfb].tick_rate` from
`~/.config/thermalwriter/config.toml` (15 FPS by default). For a session-only
60 FPS demo, change the live D-Bus property after launch:

```sh
busctl --user set-property \
  com.thermalwriter.Service \
  /com/thermalwriter/display \
  com.thermalwriter.Display TickRate u 60

thermalwriter ctl status
```

The status output should show `mode: xvfb`, the expected resolution, and
`tick_rate: 60`. Returning to a layout restores the pre-stream layout rate.

## Keyboard access through VNC

A controller talks directly to SDL and does not require VNC. For keyboard/menu
access, expose only the private Xvfb display on loopback. In a separate
terminal:

```sh
AUTH=$(find "$XDG_RUNTIME_DIR/thermalwriter" \
  -mindepth 2 -maxdepth 2 -name Xauthority -print -quit)
DISPLAY_NUM=$(pgrep -a Xvfb | grep -F -- "$AUTH" | awk 'NR == 1 { print $3 }')

test -n "$AUTH" && test -n "$DISPLAY_NUM"
env -u WAYLAND_DISPLAY x11vnc \
  -display "$DISPLAY_NUM" -auth "$AUTH" \
  -nopw -localhost -forever -shared
```

Then connect from another terminal:

```sh
vncviewer localhost:0
```

TigerVNC shortcuts:

- `Ctrl+Alt+Enter`: toggle fullscreen.
- `Ctrl+Alt+G`: grab or release the keyboard.
- `F8`: open the viewer menu.

TigerVNC/x11vnc does **not** provide a true relative-pointer grab. VNC sends
absolute pointer coordinates, so it cannot provide unrestricted FPS mouse-look.
Keep `in_nograb 1`; use the gamepad for reliable camera control. Keyboard-only
fallbacks are WASD for movement, Left/Right to turn, and Delete/Page Down to
look vertically.

The `-nopw` server above is safe only because `-localhost` prevents remote
connections. Stop x11vnc with `Ctrl+C` when finished.

## Stop and restore the layout

```sh
thermalwriter ctl layout svg/neon-dash-v2.svg
```

This terminates dhewm3 and the private Xvfb process group, restores the normal
layout, and restores the pre-stream layout FPS. Stop the foreground x11vnc
terminal as well; its viewer will disconnect when Xvfb exits.

## Troubleshooting

**Only the main menu is clean; 3D scenes contain black triangles or repeated
geometry** — confirm the launch includes `+set r_shadows 0`. The corruption is
visible in a lossless Xvfb capture and is unrelated to JPEG quality.

**Mouse points down or spins continuously** — confirm the launch includes
`+set in_nograb 1`. Do not try to restore relative mouse mode through VNC.

**The controller is detected but does nothing** — dhewm3 has no default game
bindings. Confirm `$D3_SAVE/base/gamepad.cfg` exists and the launch contains
`+exec gamepad.cfg`. The console should print `execing gamepad.cfg`.

**The controller is not detected** — turn it on and confirm Linux exposes a
joystick/event device, not only the wireless dongle:

```sh
ls -l /dev/input/by-id | grep -iE 'joystick|gamepad|controller|8bitdo'
grep -i -A12 -B3 -E 'gamepad|controller|8bitdo' /proc/bus/input/devices
```

**The window falls back to 640×480** — use windowed custom mode exactly as
shown (`r_fullscreen 0`, `r_mode -1`, custom width/height, position `0,0`).
