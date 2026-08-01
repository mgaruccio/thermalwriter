# Hardware validation

Guided `thermalwriter validate-device` runs collect shareable evidence for one connected LCD. Use them to exercise the validation pipeline safely before promoting a hardware fingerprint to **Tested** in the README.

Passive mode (`--passive`) inventories USB descriptors and correlates hidraw without opening the device for handshake or frame output. Active mode (default when `--passive` is omitted) stops the user daemon when needed, negotiates the profile, sends deterministic test cards, runs an interactive visual checklist, soaks, and reconnects.

Build from source with default features (`daemon` is on by default). Release tarballs ship the same binary.

## Evidence tiers (README Supported Devices)

The README groups coolers at the **hardware-fingerprint / profile** level (VID:PID plus negotiated PM/SUB, `bcdDevice`, and transport shape where relevant).

| Tier | Meaning |
| --- | --- |
| **Tested** | A maintainer-reviewed **full guided pass** (`validate-device` active run with `result = "pass"`) for that **exact** fingerprint and profile. Isolation, visual checks, soak, reconnect, and daemon restoration are all part of the gate. |
| **Likely** | Public upstream evidence and/or local partial runs (enumeration, frame delivery without the full checklist, upstream registry rows) — **not** a substitute for the guided pass. |
| **Untested** | Protocol/fixture mapping exists in code without adequate physical evidence for that fingerprint. |

**Never promote to Tested from:**

- Null transport / `THERMALWRITER_TRANSPORT=null` runs
- Fixture or capture-pending profiles
- Passive-only (`--passive`) reports
- Active runs that ended in `fail` or `aborted`
- Synthetic preview/matrix renders (`preview_layout --matrix`, etc.)

Only a maintainer-reviewed **pass** report moves a row from Likely/Untested to Tested.

## Command reference

```text
Guided hardware validation for a connected LCD (passive preflight today)

Usage: thermalwriter validate-device [OPTIONS] --device <DEVICE>

Options:
      --device <DEVICE>            VID:PID e.g. 0416:5302
      --bus-address <BUS_ADDRESS>  Required when duplicate VID:PIDs; always converted internally to explicit bus/address
      --passive
      --output <OUTPUT>            [default: validation-results]
```

Examples:

```sh
# Passive preflight (read-only inventory + descriptor capture)
cargo run -- validate-device --device 0416:5302 --passive

# Active guided pass (known-working bulk regression)
cargo run -- validate-device --device 87ad:70db

# Active guided pass (HID Type 2 — only after passive + regression review)
cargo run -- validate-device --device 0416:5302

# Duplicate VID:PID on the bus
cargo run -- validate-device --device 0416:5302 --bus-address 1:14 --passive
```

Installed binary: `thermalwriter validate-device …` (same flags).

## Ordered validation gates

Run these in order when bringing up a new HID Type 2 unit (for example Trofeo Vision `0416:5302`):

1. **Passive preflight** on the candidate — no handshake, no frame output:

   ```sh
   cargo run -- validate-device --device 0416:5302 --passive
   ```

2. **Known-working full regression** on hardware you already trust (local Peerless bulk unit):

   ```sh
   cargo run -- validate-device --device 87ad:70db
   ```

   Confirms the validator, daemon stop/restore, cards, soak, and reconnect path on a profile that already delivers frames.

3. **Active pass on the new unit** — only after reviewing bundles from (1) and (2):

   ```sh
   cargo run -- validate-device --device 0416:5302
   ```

   Do not skip (1) or (2) because an active run performs real USB I/O and holds the device for several minutes.

For bulk-only IDs (for example `87ad:70db`), step 1 is optional when you already know the descriptor shape; step 2 is still the primary regression.

## Passive mode

Passive validation:

- Enumerates matching USB devices via libusb
- Selects exactly one device (see [Device selection](#device-selection))
- Correlates hidraw via sysfs when the descriptor has HID shape
- Records fingerprint, pre-handshake policy, and passive allowlist status
- Writes `report.toml`, `descriptor.txt`, and `validator.log` under the output directory
- Does **not** stop the daemon, handshake, or send frames

HID interrupt **IN-only** descriptors are valid. A missing interrupt OUT endpoint in the USB descriptor does **not** block passive allowlist entry for the `0416:5302 / bcdDevice 4.07` HID-in shape.

## Active mode

Active validation runs the full state machine:

1. Passive preflight stages (inventory, selection, hidraw correlation, allowlist)
2. **Exclusive ownership** — see [Daemon and device ownership](#daemon-and-device-ownership)
3. **Handshake / negotiation** — protocol-specific; records negotiated PM/SUB in the report (never assume them beforehand)
4. **Conservative stop** when negotiation does not authorize active writes (see [Type 2 `4.07` policy](#type-2-bcddevice-407-policy))
5. **Test cards** — three encoded frames plus expected PNG previews
6. **Interactive prompts** — target marker, second-display check, orientation, colors
7. **Soak** — default 300 s (~5 min) at ~5 FPS with a looping frame
8. **Reconnect** — operator unplugs/replugs; validator resolves the new bus/address
9. **Daemon restore** — restarts the user unit only if it was active before acquisition

Active runs require a **visible terminal** for yes/no prompts and the reconnect step. Default soak is five minutes.

### Type 2 `bcdDevice 4.07` policy

For `0416:5302` with `bcdDevice 4.07` and correlated hidraw, the validator selects the **read-only probe** path before any output:

- Bounded HID read (capacity 512 bytes; unrelated to the 8-byte endpoint max packet size)
- Negotiated PM/SUB and output route are taken from the probe response — **do not document or assume PM58, PM68, or any other profile for your local unit before negotiation**
- If negotiation yields a profile that does **not** authorize active writes (anything other than the evidenced PM58/SUB0 HID-report route on that probe), the run **conservative-stops** before sending frames: `negotiated profile blocks active output (conservative stop)` / `negotiated profile does not authorize active writes`
- Upstream PM58/SUB0 evidence on one 4.07 unit ([TRCC #228](https://github.com/Lexonight1/thermalright-trcc-linux/issues/228) / [PR #230](https://github.com/Lexonight1/thermalright-trcc-linux/pull/230)) must not be generalized to other profiles sharing the same VID:PID or firmware BCD

Bulk `87ad:70db` uses the legacy bulk init handshake path when the descriptor matches.

## HID length fields (independent)

Several byte counts appear in validation reports; they are **not** interchangeable:

| Field | Typical value (Type 2) | Role |
| --- | --- | --- |
| Endpoint max packet size | 8 (interrupt IN) | USB descriptor wire limit per transaction |
| Logical HID report size | From descriptor / driver | Kernel HID report length |
| Protocol chunk | 512 | Upstream Type 2 payload per report chunk |
| Userspace submit | 513 | Report ID byte (0) + one protocol chunk on hidraw `write(2)` |
| Transport return | 513 | Bytes returned from `write(2)` for a full submit |
| Probe read capacity | 512 | Bounded userspace read for the 4.07 passive/active probe |
| Protocol response bytes | From observation | Parsed response length (8 short or longer legacy) |

Output on HID-report routes may use **interrupt OUT** or **control SET_REPORT on EP0** via the kernel hidraw driver. Descriptor-level interrupt OUT is **not** required.

## Device selection

- `--device` must be `VID:PID` (hex, for example `0416:5302`). Config aliases like `auto` are not accepted.
- With a single matching device, that bus/address is selected automatically.
- With **multiple** devices sharing the same VID:PID, pass `--bus-address BUS:ADDRESS` (decimal, for example `1:14`). Without it, selection fails with an ambiguous duplicate error.
- Internally, all I/O targets that explicit bus/address.
- For HID shapes, hidraw correlation walks `/sys/class/hidraw` and matches USB ancestors to the selected bus/address. Bulk-only shapes skip hidraw correlation.

List candidates before running:

```sh
lsusb
# optional: ls -l /sys/class/hidraw/
```

## Daemon and device ownership

Active validation acquires exclusive access before handshake or output:

- Checks whether `com.thermalwriter.Service` is owned on the session bus
- Scans `/proc/*/fd` for open handles on the target hidraw or `/dev/bus/usb/…` node — **no PID guessing or broad `kill`**
- If the `thermalwriter.service` user unit is **active**, runs `systemctl --user stop` (never `enable`/`disable`)
- On exit, runs `systemctl --user start` **only** if the unit was active before acquisition

Passive `--passive` runs do not use this guard.

## Output bundle

Each run writes a timestamped directory under `--output` (default `validation-results/`):

```text
validation-results/0416-5302-4-07-2026-07-31T23-00-00/
  report.toml          # shareable validation report (when serialization succeeds)
  descriptor.txt       # human-readable interface/endpoint summary
  validator.log          # stage log (may contain local paths — redact before public upload)
  expected-target-marker.png   # active only
  expected-orientation.png     # active only
  expected-colors.png          # active only
```

Directory mode `0700`, files `0600`.

### Shareable `report.toml`

Shareable serialization omits:

- USB serial **values** (`serial_present` boolean is kept)
- Bus and address
- `/sys/` paths and home-directory paths

Review `validator.log` and `descriptor.txt` before attaching to a public issue; strip any remaining local paths manually.

### Results: pass, fail, aborted

| `result` | Meaning |
| --- | --- |
| `pass` | All required checks succeeded; only this outcome can promote Tested **after maintainer review** |
| `fail` | Validator detected a failed check or operator negative visual confirmation |
| `aborted` | Operator declined a prompt or peer display identity changed after reconnect |

Failed and aborted bundles are still valuable for triage — attach them to device reports with the stage and error fields from `report.toml`.

## Safety rules

- **No USB bus reset** and **no global `usbhid` unload** — the validator does not perform these; do not run them manually during a pass.
- **Target one display** — when a second supported LCD is attached, the validator snapshots peer identities and asks whether the **second display stayed unchanged**. Do not aim the run at the wrong cooler; unplug extras when possible.
- **Do not run active validation on units you cannot afford to disturb** — the daemon stops for the duration and frames overwrite the LCD content.
- **Passive first** on unknown `0416:5302 / 4.07` shapes before active I/O.
- **Conservative stop is expected** for profiles the codebase has not authorized for output — treat it as signal, not a bug.

## Long interactive runs (maintainers and agents)

Active validation defaults to **300 seconds** of soak (`THERMALWRITER_VALIDATE_SOAK_SECS` overrides when set). Total wall time is often **well over five minutes** with prompts and reconnect.

- Run in a **visible terminal** (manual session or herdr pane) so prompts and the reconnect step are answered promptly.
- **Agents and CI** must not block a foreground shell on these runs. Background long jobs per the long-running-jobs skill (`/home/mike/.pi/agent/skills/long-running-jobs/SKILL.md`): herdr when `HERDR_ENV=1`, otherwise tmux plus a log file.
- Do not claim a physical pass without the operator-attested prompts and a `result = "pass"` report from that run.

## Optional environment

| Variable | Effect |
| --- | --- |
| `THERMALWRITER_VALIDATE_SOAK_SECS` | Soak duration in seconds (default `300`) for active runs |

Hardware-free daemon development (`THERMALWRITER_TRANSPORT=null`) does not substitute for `validate-device`; use null transport for layout and fixture work only.

## Related docs

- [Troubleshooting](troubleshooting.md) — udev, permissions, multiple displays
- [Configuration](configuration.md) — `display.device` selection for normal daemon use
- README **Supported Devices** — tier tables updated only after reviewed pass reports
