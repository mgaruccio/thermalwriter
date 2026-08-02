# Second-screen / dual-display state (2026-08-02)

Handoff document for Trofeo Vision wide panel work. **Do not assume the wide
panel reliably displays frames** — USB I/O succeeds and all later synchronized,
operator-watched holds stayed blank, but the operator did see the glass change
colors during the original setup sequence. That earlier positive observation must
be reproduced rather than dismissed.

---

## Hardware

| Role | USB ID | Wire | Negotiated | Physical status |
|------|--------|------|------------|-----------------|
| Primary (working) | `87ad:70db` | bulk | PM4/SUB5 FBL72 **480×480** JPEG | Works with stock daemon |
| Secondary (intermittent/unresolved) | `0416:5302` | HID Type2 | PM128/SUB1 FBL128 **1280×480** JPEG, bcdDevice **4.07** | Enumerates, handshakes, accepts writes; changed colors during the original setup but all later watched holds stayed blank |

- Local product family: Thermalright Trofeo Vision–class 6.86″ wide LCD (same
  VID:PID/PM as TRCC “A1LM24” / Trofeo Vision testers).
- HID report descriptor (35 bytes): vendor page `FF06`, **36-byte input**,
  **512-byte output**, 8-byte feature; unnumbered reports; EP OUT `0x02` mps
  512 interrupt, EP IN `0x83` mps 8 interrupt.
- Second interface is vendor-class empty (no endpoints).

---

## Operator-confirmed glass results

| Observation | Result |
|-------------|--------|
| Long-running production hold (magenta + green/cyan corners, 90° payload, hidraw, continuous ~5 Hz) | **Still blank** (user, 2026-08-02) |
| Clean replug; normal PM128 init; RGB card as JPEG SOF 1280×480 + header 1280×480; one aligned libusb transfer at 1 Hz for 30 s | **Still blank** (user watched live, 2026-08-02) |
| Exact TRCC Linux v6.1.4 tag/commit, native PyUSB Type2 handler, native Pillow JPEG defaults, solid magenta at 1 Hz for 30 s | **Still blank**; all 30 `send_frame` calls returned true (user watched live, 2026-08-02) |
| Clean replug; no init; hidapi open; planned 3 s settle then `0x00` + 512-byte reports | **Transport failed before first frame**; device disconnected/re-enumerated during settle, first write returned `-1`; no glass conclusion |
| Original setup sequence at ~20:58 on 2026-08-01 | Operator later reported that the wide glass **turned colors during setup**. The session did not synchronize each command with an observation, so the exact triggering write/color is unknown; this is still positive glass evidence and must not be relabeled as a boot flash without proof. |
| Square `87ad:70db` | Continues to work |

**Conclusion:** Do not treat “USB write OK” as display OK, but also do not treat
the later blank holds as proof that the glass never responded. The immediate goal
is to reproduce the exact original setup state while the operator watches, one
step at a time. A known-good Windows/TRCC capture remains the fallback if that
sequence cannot be reproduced.

### Exact setup sequence correlated with the earlier color changes

The prior session transcript preserves the relevant commands. At approximately
20:58 on 2026-08-01, before the later daemon and rotation changes, the sequence
was:

1. Stop the experimental dual daemon.
2. Confirm `0416:5302` on USB, restore the kernel `usbhid` binding, and obtain
   `/dev/hidraw0` for that exact device.
3. Open `/dev/hidraw0` once with `O_RDWR`.
4. Write an unnumbered 513-byte HID output report: leading report ID `0x00`,
   followed by a 512-byte Type2 init beginning `DA DB DC DD`, with byte 12 set
   to `0x01`.
5. After only 50 ms, read the 36-byte response
   `da db dc dd 01 80 ... 01 ...`, confirming PM128/SUB1.
6. Build a Pillow/JFIF solid-red JPEG at **1280×480**, quality 85 (then a
   second 25-second run at quality 90). Build the normal 20-byte Type2 header:
   command `0x02`, JPEG mode `0`, declared width 1280, height 480, frame marker
   byte 12=`0x02`, and the unpadded JPEG length.
7. Pad header+JPEG to a 512-byte boundary. Submit each 512-byte protocol chunk
   as a separate 513-byte hidraw report with the `0x00` report-ID prefix.
8. The first run sent 12 frames about 350 ms apart; the immediately following
   red hold sent 95 frames about 250 ms apart for 25 seconds.

The transcript records full 513-byte init acceptance, the full 36-byte PM128
reply, and all 21 reports per frame accepted. The operator's later statement
that the panel changed colors during setup is temporally correlated with this
sequence, but because no one asked the operator to watch and acknowledge each
step, it does **not** prove whether the red JPEG itself appeared, whether another
color appeared, or whether init/rebind/re-enumeration changed the panel state.

Important deltas introduced immediately afterward include handing the device to
the dual daemon, changing from the one-off script to production encoding, later
forcing a 90° payload rotation, and repeated unbind/rebind/re-enumeration. The
original sequence—not the later guessed variants—is the highest-priority replay.

### 2026-08-02 re-evaluation: the last “fix” likely encoded the wrong raster

The best current explanation is not that drawing failed. The host-side test image
was drawn correctly, but commit `95b162f` then rotated the 1280×480 RGB canvas
by +90° and produced a **480×1280 JPEG** while retaining **1280×480 in the
Type2 header**. Successful HID writes only prove that the kernel accepted those
reports; they do not prove that firmware accepted this internally inconsistent
frame.

Fresh upstream evidence changes the next step:

- A PM128/FBL128 owner reported **working horizontal themes with TRCC 6.1.4**.
  That version's PM128 path used JPEG with no device pre-rotation, so JPEG SOF
  and Type2 header were both 1280×480.
- The current C# oracle extraction also assigns 1280×480 to the ordinary
  **20-byte `DA DB DC DD` JPEG header**, not the 64-byte `12 34 56 78` header.
- Upstream's later widescreen audit says the 1280×480 encode base at orientation
  0 is 0°. The blanket +90° solid-color transform used to justify `95b162f` is
  not the known-good horizontal-theme path.
- TRCC 9.9.5 is not a clean oracle for this local unit: it applies PM58 Frozen
  Warframe SE quirks to every `0416:5302` with bcdDevice 4.07 before PM is known.
  On this PM128 device that means skip-init and portrait-native 480×1280 behavior
  intended for a different 240×320 panel.

**Revised diagnosis after the watched 2026-08-02 test:** the forced +90°
SOF/header mismatch was a real defect, but it is **not the sole reason this local
glass is blank**. A clean-session replay with normal PM128 init, JPEG SOF
1280×480, Type2 header 1280×480, and one logical libusb frame transfer also
completed 30 times while the operator saw no change.

**Evidence classification**

- **Confirmed locally:** the target re-enumerated at bus 3 address 22 after a
  physical replug; no process owned `/dev/hidraw0`; the normal init returned the
  full 36-byte PM128/SUB1 response.
- **Confirmed locally:** the retained packet uses the 20-byte DA JPEG header,
  JPEG SOF 1280×480, header 1280×480, payload length 23,519, aligned packet
  length 23,552. Thirty full-length interrupt transfers completed at 1 Hz.
- **Confirmed on glass:** the operator watched the full hold and saw nothing.
  Therefore matched dimensions and successful libusb transfer are insufficient.
- **Confirmed upstream, not reproduced locally:** another PM128/SUB1 owner
  displayed horizontal themes with this general frame shape. Some unrecorded
  difference remains: session/init behavior, exact JPEG bytes, firmware state,
  or another protocol field/path.
- **Not claimed:** the panel is fixed or physically faulty. The test disproves
  only the dimension-mismatch-as-complete-explanation hypothesis.

---

## Current machine config (as of this write)

- `~/.config/thermalwriter/config.toml`: **single display**
  `device = "87ad:70db"` only (dual `[[displays]]` removed so primary stays reliable).
- Backup of earlier dual attempt:
  `~/.config/thermalwriter/config.toml.bak-second-screen`
- User systemd `thermalwriter.service`: **active**, drives square panel only.
- Installed binary refreshed once from release build to
  `~/.cargo/bin/thermalwriter` (needed so old binary no longer aborts whole
  scan when `0416:5302` is plugged in).
- Hold jobs should be **stopped** after confirmed blank; primary stays up.

Example dual config that **connected both** in software (but secondary glass blank):

```toml
[[displays]]
device = "87ad:70db"
default_layout = "svg/neon-dash-v2.svg"
mode = "svg"
rotation = 180

[[displays]]
device = "0416:5302"
default_layout = "svg/solid-test.svg"   # or neon-dash
mode = "svg"
rotation = 0
```

Status when dual ran: `connected=true`, `display_count=2`, primary resolution
`480x480`, independent tick `rotations=[180, 0]`, HID frames logged as sent.

---

## What works in software

1. **Discovery** of HID Type2 interrupt endpoints (no longer skipped as
   “interrupt-only”).
2. **Handshake** via hidraw or libusb: Type2 init → 36-byte reply
   `da db dc dd 01 80 … 01 …` → PM=128 SUB=1 FBL=128, 1280×480 JPEG profile.
3. **Independent multi-display config** `[[displays]]` opens both devices,
   separate frame sources, per-output rotation.
4. **Frame pipeline** builds Type2 packets, 512-aligned, continuous send loop
   without transport errors (thousands of frames).
5. **validate-device** passive + active negotiate:
   artifact
   `validation-results/0416-5302-4-07-2026-08-01T20-37-20/`
   - handshake **pass**
   - `profile_policy = "observed_inactive"`
   - **conservative stop** before guided cards (no authorized active-write
     policy for PM128 — only PM58/SUB0 short-response is write-authorized).

---

## What does *not* work on glass

Every later synchronized or long-running hold that the operator explicitly
checked left the panel blank (boot logo / black), including:

| Path | Notes |
|------|--------|
| Python hidraw, report-ID `0x00` + 512 chunks, solid JPEG landscape | Writes OK |
| Python hidraw, same, various colors / 480×1280 JPEG | Writes OK |
| Python pyusb interrupt (no report ID), 90° rotate JPEG, native header dims | Writes OK |
| Production `send_test_frame` / dual daemon, hidraw preferred | Writes OK; encode later forced **480×1280** payload |
| Production path after 90° fix (`encoded 480x1280`) continuous hold | **User: still blank** |
| TRCC 9.9.5 `display color` (also rotates 90°, ~17–20 KB Type2 packet) | Software “Sent N bytes”; **glass not user-confirmed** |

So the remaining problem is **payload/protocol acceptance by firmware**, not
“daemon not running” or “device not opened”.

---

## TRCC and upstream reference

> The local `/tmp/trcc-venv` contains TRCC 9.9.5, but that version is not a
known-good PM128 reference on bcdDevice 4.07.

### Direct PM128 evidence

- Upstream issue #1 records this exact negotiated family: PM128/SUB1/FBL128,
  A1LM24, 1280×480.
- In the same issue, the original blank-screen failure was fixed in v5.2.0 by
  changing RGB565/240×320 framing to **JPEG with actual 1280×480 header dims**.
- The reporter then confirmed on TRCC 6.1.4: **“Screen runs fine with horizontal
  Themes.”** That path's JPEG encoder did not pre-rotate orientation-0 content.
- C# oracle commit `b2866d2` mines the 1280×480 `ImageToJpg` case as a 20-byte
  `DA DB DC DD` header with declared width 1280, height 480, payload length at
  bytes 16..20. The commit found 64-byte headers for several other resolutions,
  but its generated oracle does **not** assign one to 1280×480.
- Rotation audit `e8d6b30` fixes the 1280×480 encode base at 0° and makes it
  SUB-independent. At user orientation 0, the expected wire JPEG remains
  landscape 1280×480.

### Why TRCC 9.9.5 misled this session

- Its solid-color helper applies a blanket +90° whenever `profile.rotate` is
  true. That differs from its normal orientation-0 widescreen theme angle.
- Its quirk registry keys only `(VID, PID, bcdDevice)`. Therefore this PM128
  4.07 unit receives the PM58 Frozen Warframe SE flags: HID reports, skip init,
  short handshake, portrait-native, and keepalive.
- If the 36-byte PM128 reply is read, `portrait_native` transposes the effective
  profile to 480×1280 and disables rotation. This is not the old confirmed
  PM128 horizontal-theme path.
- The local unit's 36-byte init response is materially different from the PM58
  quirk source's unsolicited 8-byte response. Reusing the PM58 session rules is
  a hypothesis, not evidence.

### Still-valid common facts

- HID report descriptor: 512-byte output, 36-byte input; unnumbered reports.
- Linux hidraw writes require a leading report ID `0x00`; the kernel uses the
  interrupt OUT endpoint when present and otherwise falls back to `SET_REPORT`.
- Type2 data is 512-aligned. Whether this PM128 firmware distinguishes one
  multi-packet transfer from sequential 512-byte report submissions remains a
  useful second-order A/B test.

---

## thermalwriter commits on this branch (second-screen related)

On `master` (ahead of origin; verify with `git log`):

| Commit | Summary |
|--------|---------|
| `327ef9c` | `feat: independent multi-display via [[displays]]` |
| `98ebaaa` | Type2 4.07 probe: silent read then init elicit |
| `2b3f8d7` | validate: allow legacy-length inactive probe replies |
| `4902009` | discovery: enumerate HID Type2 interrupt endpoints |
| `59d5223` | docs: local PM128/SUB1 note |
| `70b281e` | Type2 prefer hidraw report-ID path; 512-chunk frames |
| `95b162f` | HID Type2 widescreen JPEG: +90° wire rotate; native header dims; drop FBL128 sub hacks |

Uncommitted noise often present: rustfmt-only touches under `validate_device/`,
`guard.rs`, and a local `examples/send_test_frame.rs` hold harness (restore
with `git checkout -- examples/send_test_frame.rs` if needed).

---

## Validation policy gap

`authorize_hid_report_writes` / active validate only authorizes **PM58/SUB0
short 8-byte** responses. PM128/SUB1 legacy 36-byte negotiate is recorded as
`observed_inactive` → guided cards/soak never run. That is intentional
conservatism, but it also means we have **no** validator-driven on-glass
evidence loop for Trofeo until a PM128 write policy exists **and** glass works.

---

## Remaining hypotheses after exact TRCC Linux 6.1.4 replay

1. **Local controller firmware/state differs from the upstream working unit.**
   The exact v6.1.4 PyUSB transport, Type2 handler, normal handshake, PM128
   profile, Pillow encoder defaults, 20-byte 1280×480 header, and whole aligned
   transfer all ran successfully while this glass stayed blank. Host-side Linux
   implementation differences are no longer the leading explanation.
2. **Hidden initialization outside the Linux sender.** Windows TRCC may issue a
   mode, feature, power, or panel-enable operation absent from the Linux path and
   from the C# frame-row extraction. A Windows USB capture is now more valuable
   than further guessed frame variants.
3. **4.07 reboot/session behavior.** Opening hidapi without init caused this unit
   to disconnect/re-enumerate before the first report, while normal PyUSB init
   produced a stable 30-frame session that the glass ignored. That behavior may
   distinguish this firmware revision from the upstream PM128 report.
4. **Panel/controller hardware fault.** This is now plausible: the USB MCU can
   enumerate, negotiate, and consume frames while the LCD engine, panel power,
   ribbon, or firmware display state fails. If official Windows TRCC also cannot
   display a solid color after a full power removal, hardware/firmware fault is
   the leading conclusion.

---

## Defined end-to-end test (next session; minimal thrash)

This is the required real device journey. Do not run it unattended or interpret
a successful write count as a pass.

**Environment and preconditions**

1. Keep `thermalwriter.service` on the square `87ad:70db` only; verify its config
   does not claim `0416:5302`. Stop TRCC and any ad-hoc process that could open
   `/dev/hidraw0`.
2. Ask the operator to watch the wide glass and record a short phone video.
3. Physically unplug/replug only `0416:5302`, wait at least three seconds, then
   open it exactly once. Do not USB-reset or reconnect in a loop.

**Test data**

- One 1280×480 landscape RGB card with unmistakable red/green/blue regions.
- JPEG SOF must parse as 1280×480 before sending.
- Type2 header must be the 20-byte DA header, width=1280, height=480, JPEG length
  equal to the unpadded payload; total packet padded to 512.

**Executable boundary and steps**

1. Use the production connector/encoder hold harness only after it has a named
   `landscape-native` variant (wire angle 0) and prints the parsed JPEG SOF plus
   all header fields. The current uncommitted harness is **not eligible** because
   it inherits the forced +90° transform.
2. Run one 30-second continuous hold through the real device interface while the
   operator watches. Retain stdout/stderr, the exact packet before report-ID
   wrapping, timestamps, and the phone video/photo.
3. Expected pass: the wide glass visibly changes to the card within the hold and
   remains stable; square glass remains unchanged. USB write success alone is
   only transport evidence.
4. If blank, physically replug before each single-variable A/B: first submit the
   same bytes through libusb as one logical aligned transfer versus hidraw
   512-byte reports; then compare JPEG encoder output. Do not change rotation,
   header, transport, and session policy simultaneously.
5. Cleanup: close the test handle, remove temporary packet captures, verify the
   user service still drives only `87ad:70db`, and record the observed glass
   result in this document.

**Execution result (2026-08-02 06:06:53Z): FAIL on glass.** The operator
physically replugged the target and watched the entire 30-second hold. Handshake,
packet validation, and all 30 transfers passed; the wide display remained blank.
The interface was released and the kernel HID driver reattached. One second
later the kernel logged device 22 disconnecting; it re-enumerated as device 23
after five seconds. Evidence (including the kernel USB excerpt) is retained under
`validation-results/0416-5302-4-07-2026-08-02T06-06-53-solid-landscape/`.

**No-init follow-up (2026-08-02 06:11:54Z): transport failure; not an
on-glass test.** After the operator's fresh replug, hidapi opened one session and
sent no init. The device disconnected during the three-second settle and
re-enumerated five seconds later. Report 0 on the stale handle returned `-1`, so
zero frames reached the transport. The process exited, closed the handle, and no
process owns `/dev/hidraw0`. Evidence is retained under
`validation-results/0416-5302-4-07-2026-08-02T06-11-54-no-init-hid-report/`.

**Exact TRCC Linux v6.1.4 replay (2026-08-02 06:28:33Z): FAIL on
glass.** The isolated install came from tag `v6.1.4`, commit
`a25f0f0f6ae4188eab849296794717c8d618cdbf`. Its unmodified
`PyUsbTransport`, `HidDeviceType2`, and `ImageService.to_jpeg` negotiated
PM128/SUB1/FBL128, built a 10,231-byte 1280×480 Pillow JPEG and 10,752-byte
packet, and returned true for 30 whole-transfer frames. The operator watched
the complete solid-magenta hold and saw nothing. The transport was closed and
the kernel `usbhid` driver was rebound to restore `/dev/hidraw0`. Evidence is
retained under
`validation-results/0416-5302-4-07-2026-08-02T06-28-33-trcc-v6.1.4/`.

---

## Fresh external research (2026-08-02)

Sources that changed the diagnosis:

- PM128 identity, v5.2.0 JPEG/actual-dim fix, and v6.1.4 on-glass horizontal
  success: <https://github.com/Lexonight1/thermalright-trcc-linux/issues/1>
- Current Type2 implementation and PM58-only behavioral provenance:
  <https://github.com/Lexonight1/thermalright-trcc-linux/blob/main/src/trcc/adapters/device/hid_lcd.py>
  and <https://github.com/Lexonight1/thermalright-trcc-linux/issues/228>
- C# oracle extraction (including exact 1280×480 20-byte header row):
  <https://github.com/Lexonight1/thermalright-trcc-linux/commit/b2866d29bce4ff0f6a99950efb22374d10e13175>
- Widescreen encode-base audit (1280×480 base 0):
  <https://github.com/Lexonight1/thermalright-trcc-linux/commit/e8d6b30f931cfb5c9963cf63e4827acee7964d5f>
- Linux hidraw report-ID/output behavior:
  <https://docs.kernel.org/hid/hidraw.html>

The coherent landscape replay recommended above was executed and stayed blank.
This rules out mismatched raster/header dimensions as the complete explanation
for the local unit; it does not invalidate the upstream PM128 evidence.

---

## File / path cheat sheet

| Path | Role |
|------|------|
| `docs/second-screen-state.md` | This handoff |
| `docs/configuration.md` | `[[displays]]` schema |
| `.pi/plans/2026-08-01-independent-multi-display/plan.md` | Multi-display design (gitignored under `.pi/`) |
| `validation-results/0416-5302-4-07-2026-08-01T20-37-20/` | Negotiate report (inactive) |
| `validation-results/0416-5302-4-07-2026-08-02T06-06-53-solid-landscape/` | Watched 1280×480 solid-card failure: packet, JPEG, handshake, log, hashes |
| `validation-results/0416-5302-4-07-2026-08-02T06-11-54-no-init-hid-report/` | No-init attempt: disconnect/re-enumeration before first report; no glass result |
| `validation-results/0416-5302-4-07-2026-08-02T06-28-33-trcc-v6.1.4/` | Exact known-upstream TRCC 6.1.4 implementation: 30 successful sends, watched blank glass |
| `src/transport/discovery.rs` | HID interrupt discovery |
| `src/transport/hid_lcd.rs` | Type2 hidraw/libusb, 512 chunks |
| `src/transport/encode.rs` | +90° for HID2 widescreen JPEG |
| `src/transport/profile.rs` | FBL_128 |
| `src/transport/type2_policy.rs` | 4.07 probe / PM58 authorize only |
| `/tmp/trcc-venv` | Optional TRCC 9.9.5 venv (ephemeral) |

## Latest synchronized replay and paused Windows discriminator

On 2026-08-02 at 16:43 UTC, candidate `8e4b04f` negotiated the exact
PM128/SUB1 identity and sent 538 native `1280x480` JPEG frames in 10 seconds
through one complete aligned libusb interrupt transfer per frame. The operator
watched the run and the glass remained blank. The device disconnected from USB
about one second after the libusb session closed.

After a full power cycle, the original recovered one-session hidraw sequence
was replayed as the first active session. The 513-byte init report returned the
exact 36-byte PM128 response. A quality-90 solid-red JPEG was 10,230 bytes; the
20-byte Type2 header plus JPEG padded to a 10,752-byte packet (21 reports per
frame). Forty-seven frames were sent at 250 ms intervals during a 12-second
operator-watched hold. The glass remained blank. Unlike the libusb run, the USB
controller remained enumerated afterward and continued exposing `/dev/hidraw0`.
This confirms that the USB MCU still accepts commands while the LCD engine does
not visibly update.

A Windows/KVM discriminator was then prepared but deliberately stopped before
installation at the operator's request. Official Thermalright download metadata
lists Trofeo VISION under TRCC 2.1.6. The official Windows 11 Enterprise
evaluation ISO, TRCC archive/tools ISO, an 80 GiB qcow2 disk, UEFI/TPM support,
and exact `0416:5302`-only USB passthrough scripts remain under
`/home/mike/vm/windows-test/`. No Windows or official-TRCC on-glass test was
completed. QEMU was stopped, the wide device was released, and the existing
square-only service was not changed.

---

## Bottom line

- The physical USB data path works: discovery, PM128 negotiation, and sustained
  complete frame transfers are repeatable.
- The exact upstream TRCC Linux 6.1.4 PM128 implementation also completed 30
  sends while the local glass stayed blank. Geometry, Pillow JPEG defaults,
  Type2 header construction, and normal PyUSB delivery are not sufficient
  explanations for those later failures.
- The original setup nevertheless produced operator-observed color changes. The
  transcript ties those changes most closely to an immediate `usbhid`/hidraw
  restore, normal Type2 init and 36-byte reply, followed without a long settle
  by native 1280×480 JPEG reports. It does not prove which command caused the
  visible change, but it rules out the claim that the glass never responded.
- The operator-synchronized original hidraw replay has now been repeated after a
  full power cycle and remained blank despite the exact PM128 response and 47
  accepted solid-red frames.
- The remaining discriminator is official Windows TRCC after full device power
  removal, ideally with a USB capture. The VM assets are prepared but the Windows
  test has not been run. If official TRCC also stays blank, panel/controller
  hardware becomes the leading cause.
- Do not re-enable dual config or claim support until an on-glass test passes.
