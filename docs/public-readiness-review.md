# Public Readiness Review

Date: 2026-07-01

Verdict: **repository blockers resolved; attached-hardware smoke checks passed; broad announcement still needs clean-VM install/replug verification**.

All fourteen review issues below have repository-side fixes recorded in their sections. An attached Thermalright `87ad:70db` cooler was detected and the repo-built daemon successfully reported it over D-Bus. Remaining launch risk is clean-host validation: installer behavior from scratch, udev application after replug, physical detach/reattach reconnect, and tag-triggered release packaging.

## Scope Reviewed

Reviewed public/share surfaces and runtime install paths:

- `README.md`
- `CHANGELOG.md`
- `CONTRIBUTING.md`
- `SECURITY.md`
- `LICENSE`
- `Cargo.toml`
- `docs/release.md`
- `docs/configuration.md`
- `docs/gui.md`
- `.github/workflows/ci.yml`
- `.github/workflows/release.yml`
- `packaging/install.sh`
- `packaging/uninstall.sh`
- `packaging/udev/99-thermalwriter-rapl.rules`
- `systemd/thermalwriter.service`
- `src/main.rs`
- `src/cli.rs`
- `src/config.rs`
- `src/transport/bulk_usb.rs`
- `src/service/dbus.rs`
- `src/service/xvfb.rs`
- `src/service/frame_dump.rs`
- `gui/src-tauri/src/commands.rs`
- `gui/src/lib/streamPresets.ts`
- representative docs/plans and docs/brainstorms public hygiene markers

## Verification Performed

| Check | Result |
|---|---:|
| `cargo test --workspace` | Passed: 279 tests, 1 ignored |
| `cargo test --workspace --no-default-features` | Passed: 178 tests, 1 ignored |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed |
| `cd gui && npm ci && npm run build` | Previously passed; not rerun in this fix pass |
| `cargo package --list -p thermalwriter --allow-dirty` | Passed; package contents include README-linked skill guide |
| `cargo package -p thermalwriter --allow-dirty` | Passed; packaged 89 files and verified successfully |
| Secret-like token scan | No matches found |
| `lsusb -d 87ad:70db` | Passed: attached `ChiZhu Tech USBDISPLAY` found |
| Installed `systemctl --user status thermalwriter.service` | Passed: active user service connected to `/home/mike/.cargo/bin/thermalwriter daemon` |
| Installed `thermalwriter ctl status` | Passed: `connected: true`, `resolution: 480x480` |
| Repo-built `target/debug/thermalwriter daemon` hardware smoke | Passed: D-Bus status returned `connected: true`, `resolution: 480x480` |
| Repo-built `target/debug/thermalwriter ctl mirror /bin/sleep 10` | Passed: stream started; Xvfb dir mode `0700`, Xauthority mode `0600` |

Not verified:

- Clean distro VM install/uninstall from scratch.
- Physical USB detach/reattach reconnect transition.
- Tauri bundle packaging (`npm run tauri:build`).
- Tag-triggered GitHub release workflow.

## Severity Definitions

- **P1 / Blocker**: likely first-run failure, device incompatibility, or serious public trust issue. Fix before broad advertising.
- **P2 / High**: user-harming, security-sensitive, or likely to cause bad beta reports. Strongly fix before public beta.
- **P3 / Medium**: credibility/documentation/package hygiene. Fix before larger announcement.
- **P4 / Low**: polish and stale comments.

---

## P1 Blockers

### 1. Missing USB udev rule for the actual display device

**Status:** resolved in repository and smoke-tested against the attached `87ad:70db` cooler; clean-host udev replug acceptance remains for release QA.

**Evidence**

- The service runs as the normal user:
  - `systemd/thermalwriter.service:8` — `ExecStart=%h/.cargo/bin/thermalwriter daemon`.
- The daemon opens the raw USB display by VID/PID and claims interface 0:
  - `src/transport/bulk_usb.rs:91-101` — `rusb::open_device_with_vid_pid(0x87AD, 0x70DB)`, `claim_interface(0)`.
- The installer only invokes the RAPL setup path:
  - `packaging/install.sh:80-81` — runs `thermalwriter setup-udev`.
- The only packaged udev rule targets Intel RAPL powercap files, not USB:
  - `packaging/udev/99-thermalwriter-rapl.rules:10` — `SUBSYSTEM=="powercap", KERNEL=="intel-rapl:*" ...`.

**Impact**

On many Linux systems, a user-session service will not have permission to open or claim a raw libusb device under `/dev/bus/usb/...` unless a udev rule grants access. The advertised installer can appear to complete, restart the daemon, and then the daemon can fail before registering D-Bus because it cannot access the actual cooler.

This is the most likely "I followed the README and it does not work" failure.

**Recommended fix**

Add a USB udev rule for supported hardware, install it with the existing installer, and remove it with uninstall.

A `uaccess` rule is usually the least invasive desktop-user option:

```udev
SUBSYSTEM=="usb", ATTR{idVendor}=="87ad", ATTR{idProduct}=="70db", TAG+="uaccess"
```

Alternative: use a dedicated group if the daemon must run outside a local logind seat/session.

Update docs to mention any required replug, logout, or service restart.

**Acceptance check**

On a clean Linux machine with the cooler attached:

1. Run `./packaging/install.sh` as the normal user.
2. Replug the USB device if required by the rule.
3. Run `systemctl --user status thermalwriter`.
4. Run `thermalwriter ctl status`.
5. Confirm the daemon can open the device without `sudo` and reports the display as connected.

**Repository resolution**

- Added the `87ad:70db` USB `uaccess` rule alongside the existing RAPL rule in `packaging/udev/99-thermalwriter-rapl.rules`.
- Kept installer/uninstaller wiring through `thermalwriter setup-udev` and the existing packaged udev rule path.
- Updated install docs and setup output to tell users to replug an already-connected display after install.

---

### 2. Runtime mode switches hard-code 480x480 after negotiating device dimensions

**Status:** resolved in repository; real non-480 hardware still not available for physical acceptance.

**Evidence**

- Startup negotiates device dimensions:
  - `src/main.rs:93-99` uses `BulkUsb::new()` and `transport.handshake()`.
- Initial frame source uses negotiated dimensions:
  - `src/main.rs:140-199` uses `device_info.width` / `device_info.height`.
- Later D-Bus mode-change listener hard-codes 480x480:
  - `src/main.rs:270-272` passes `480, 480` into `build_layout_source`.
  - `src/main.rs:315-316` starts shell Xvfb at `480, 480`.
  - `src/main.rs:358-359` starts argv Xvfb at `480, 480`.
- Transport recognizes non-480 PM modes:
  - `src/transport/bulk_usb.rs:47-58` maps PM values to `240x240`, `320x320`, `320x240`, `240x320`, and `480x480`.
- README claims:
  - `README.md:11-13` — known working `87ad:70db`; other Thermalright LCD coolers using the same protocol are experimental.

**Impact**

For non-480 devices, the daemon can start with negotiated dimensions but later GUI/CLI layout switches or stream starts rebuild sources at 480x480. `BulkUsb::send_frame` still writes headers using negotiated device dimensions. The payload and header can therefore disagree after a runtime mode change.

Likely outcomes include distorted output, rejected frames, or device-specific protocol failures. This undermines the "experimental other devices" claim.

**Recommended fix**

Capture negotiated dimensions before spawning the mode-change listener and use them in every runtime source construction:

- `build_layout_source`
- `xvfb_manager::start`
- `xvfb_manager::start_argv`
- `XvfbSource::new`

If other dimensions are not actually supported yet, narrow the README claim to the known 480x480 hardware and reject unsupported PM values explicitly.

**Acceptance check**

- Add a test or injectable helper around mode-change source creation proving the listener uses negotiated dimensions, not constants.
- If possible, smoke-test a non-480 PM path with a mocked `DeviceInfo`.
- Confirm README accurately describes support.

**Repository resolution**

- Captured negotiated `RuntimeDisplayDimensions` once after USB handshake and moved it into the mode-change listener.
- The listener now constructs layout sources, shell Xvfb streams, argv Xvfb streams, and `XvfbSource` instances through that negotiated-dimension helper instead of constants.
- Added `runtime_dimensions_build_listener_layout_source`; `cargo test service::mode_handler` passed.

---

## P2 High Priority

### 3. Xvfb framebuffer and Xauthority artifacts are created under predictable shared `/tmp` paths

**Status:** resolved in repository and live-smoke-tested with repo-built daemon/Xvfb.

**Evidence**

- Xvfb temporary framebuffer directory is predictable and created under `std::env::temp_dir()`:
  - `src/service/xvfb.rs:247-254` creates `thermalwriter-xvfb-tmp-{pid}-{candidate}`.
  - `src/service/xvfb.rs:263-274` renames it to `thermalwriter-xvfb-{display}`.
- Xauthority file is created with default permissions, written and synced, then chmodded:
  - `src/service/xvfb.rs:61-75`.
- The project already treats streamed frames as sensitive elsewhere:
  - `src/service/frame_dump.rs:1-23` refuses to dump stream preview frames to shared `/tmp` and requires `$XDG_RUNTIME_DIR`.

**Impact**

On multi-user systems, predictable directories under `/tmp` can be inspected or raced. With a normal umask, the directory can be world-searchable, and the Xauthority file can briefly exist with broader permissions before chmod. Streaming can expose private window contents, so framebuffer and auth material should be as protected as the GUI preview frame path.

**Recommended fix**

- Create a private per-session temp directory with mode `0700`.
- Avoid predictable final names.
- Create `Xauthority` with mode `0600` at open time using `OpenOptionsExt::mode(0o600)`.
- Keep the framebuffer directory private for its whole lifetime, including after any rename.
- Prefer `$XDG_RUNTIME_DIR/thermalwriter/xvfb-*` over `/tmp` when available.

**Acceptance check**

- Unit-test or integration-test directory/file permissions.
- Start streaming, inspect the backing directory and Xauthority file, and verify mode `0700` / `0600`.

**Repository resolution**

- Replaced predictable `/tmp/thermalwriter-xvfb-*` allocation and final renames with fresh random framebuffer directories created mode `0700`.
- Prefer `$XDG_RUNTIME_DIR/thermalwriter/` as the private parent when available; fallback directories under `/tmp` are random leaf directories with mode `0700`.
- Create `Xauthority` with `OpenOptionsExt::mode(0o600)` and `create_new(true)` before writing cookie data.
- Added permission tests for framebuffer directory mode and Xauthority file mode; `cargo test service::xvfb` passed.

---

### 4. Public D-Bus surface can launch arbitrary commands

**Status:** resolved in repository.

**Evidence**

- CLI mirror accepts a shell command:
  - `src/cli.rs:59-63`.
- D-Bus `set_mode("xvfb", command)` accepts any non-empty command:
  - `src/service/dbus.rs:514-524`.
- The shell path executes through `sh -c`:
  - `src/service/xvfb.rs:334-343`.
- `set_mode_argv` accepts arbitrary argv:
  - `src/service/dbus.rs:608-615`.
- GUI custom preset builds arbitrary executable argv:
  - `gui/src/lib/streamPresets.ts:107-121`.

**Impact**

This is not direct privilege escalation because the daemon runs in the user's own session. Same-user processes can already run commands as that user. However, the daemon exposes command execution over a public session-bus API, and those child processes run with stdout/stderr hidden. Any same-session D-Bus client can bypass the GUI's affordances and launch long-lived local processes through thermalwriter.

For a public beta, this needs an intentional trust-boundary decision and documentation. The current surface looks accidental.

**Recommended fix**

Preferred public-beta posture:

- Remove generic shell-string streaming from the public D-Bus API.
- Expose preset IDs and validated preset arguments only.
- Keep custom command execution local to the GUI with explicit user confirmation, if retained.

If keeping arbitrary commands:

- Document the session-bus trust boundary in `SECURITY.md` and `docs/gui.md`.
- Reject relative executables and require absolute paths.
- Log command starts/stops visibly.
- Consider an allowlist or per-user config flag such as `streaming.allow_custom_commands = true` defaulting to false.

**Acceptance check**

- D-Bus introspection should not expose a generic arbitrary shell execution method unless intentionally documented.
- Tests should cover rejecting unknown commands/presets if allowlisting is implemented.

**Repository resolution**

- Disabled the legacy public `set_mode("xvfb", shell_command)` D-Bus branch; callers must use `set_mode_argv` or `start_stream_preset`.
- Changed `thermalwriter ctl mirror` to structured argv and route it through `set_mode_argv`, removing shell-string forwarding from the CLI path.
- Added validation that generic stream argv uses an absolute `argv[0]`; built-in presets resolve executable names to absolute paths via the daemon's `PATH`.
- Documented the same-user session-bus trust boundary and streaming process behavior in `SECURITY.md`, `docs/gui.md`, and README usage notes.
- Added rejection and argv validation tests; `cargo test service::dbus` and `cargo test cli::tests::cli_parses_ctl_mirror` passed.

---

### 5. Daemon restart-loops when hardware is absent

**Status:** resolved in repository; startup with connected hardware was smoke-tested, while physical unplug/replug acceptance remains for release QA.

**Evidence**

- Daemon opens and handshakes USB before registering D-Bus:
  - `src/main.rs:93-95`.
- D-Bus service starts later:
  - `src/main.rs:240-242`.
- Unit restarts on failure:
  - `systemd/thermalwriter.service:8-10`.
- Installer always restarts the service:
  - `packaging/install.sh:83-85`.

**Impact**

If a user installs before plugging in hardware, the daemon exits before registering D-Bus. `thermalwriter ctl status` cannot report a useful disconnected state because the service never came up. systemd then restarts it repeatedly.

README does list a supported cooler as a requirement, but a public installer should degrade gracefully when the device is temporarily absent.

**Recommended fix**

- Start D-Bus regardless of initial USB presence.
- Initialize state as `connected=false` if open/handshake fails.
- Let the tick loop or a reconnect task retry USB open later.
- `thermalwriter ctl status` should work without hardware and report disconnected.

**Acceptance check**

- On a machine without the cooler, `thermalwriter daemon` should stay running and expose D-Bus.
- `thermalwriter ctl status` should return `connected: false`.
- Plugging in the cooler should transition to connected without restarting the service.

**Repository resolution**

- Startup now creates a disconnected `BulkUsb` placeholder when initial open/handshake fails, so D-Bus startup is no longer gated on hardware presence.
- `ServiceState.connected` and the connected watch channel are initialized from actual startup hardware state.
- The existing tick-loop reconnect path now retries from the disconnected placeholder and flips `connected=true` after a successful later handshake.
- Added `disconnected_bulk_usb_starts_unconnected_and_reconnectable`; targeted transport and mode-handler tests passed.

---

### 6. Installed binary path can diverge from systemd `ExecStart`

**Status:** resolved in repository; full service start still requires running the installer in a user systemd session.

**Evidence**

- Installer uses `${CARGO_HOME:-$HOME/.cargo}/bin`:
  - `packaging/install.sh:14`.
- Release binary is installed to `$CARGO_BIN/thermalwriter`:
  - `packaging/install.sh:66-72`.
- Unit hard-codes `%h/.cargo/bin/thermalwriter`:
  - `systemd/thermalwriter.service:8`.

**Impact**

If a user has `CARGO_HOME` set to a non-default location, install writes a valid binary to one path while systemd executes another path. The service fails at first start and every boot.

**Recommended fix**

Pick one invariant:

1. Always install to `$HOME/.cargo/bin/thermalwriter`, ignoring `CARGO_HOME`, because the unit hard-codes that path.
2. Or generate the unit during install with the actual installed binary path.

Option 2 is more correct if honoring `CARGO_HOME` is intentional.

**Acceptance check**

- Run installer with `CARGO_HOME` set to a temporary custom path.
- Confirm `systemctl --user cat thermalwriter` points to the exact installed binary.
- Confirm service starts.

**Repository resolution**

- Installer now records `INSTALLED_BIN="$CARGO_BIN/thermalwriter"` and verifies it exists after install.
- Installer generates the user unit at install time with `ExecStart` pointing to that exact binary path, preserving custom `CARGO_HOME`.
- README release install text now describes the generated unit behavior.
- `bash -n packaging/install.sh` passed.

---

### 7. Source install docs omit native build prerequisites enforced by installer

**Status:** resolved in repository.

**Evidence**

- README requirements list Linux/systemd/udev, Rust, hardware, D-Bus, optional Node, optional Xvfb:
  - `README.md:27-34`.
- Installer exits if `pkg-config` is missing:
  - `packaging/install.sh:50-53`.
- Installer exits if libudev development files are missing:
  - `packaging/install.sh:55-58`.

**Impact**

A clean Debian/Fedora/Arch user following the README source install can fail before Cargo builds. The installer error is descriptive, but the README should set expectations before the user starts.

**Recommended fix**

Add distro package examples to README:

```sh
# Debian / Ubuntu
sudo apt install pkg-config libudev-dev

# Fedora
sudo dnf install pkgconf-pkg-config systemd-devel

# Arch
sudo pacman -S pkgconf systemd
```

Also mention the GUI dependencies separately if documenting local Tauri packaging.

**Acceptance check**

README source install section names the native dependencies that `install.sh` checks.

**Repository resolution**

- README requirements now list `pkg-config`/`pkgconf` and libudev development packages for Debian/Ubuntu, Fedora, and Arch, matching the checks in `packaging/install.sh`.

---

### 8. `setup-udev` deletes another project's tmpfiles rule

**Status:** resolved in repository.

**Evidence**

- `src/cli.rs:232` defines `STALE_TRCC_TMPFILE = "/etc/tmpfiles.d/trcc-rapl.conf"`.
- `src/cli.rs:285-288` removes that file if it exists.

**Impact**

`thermalwriter setup-udev` runs with root privileges. If `trcc` is still installed or the user intentionally kept that tmpfiles rule, thermalwriter deletes another application's configuration.

**Recommended fix**

- Do not delete foreign files automatically.
- Warn with manual remediation instructions instead.
- Only remove the file automatically if thermalwriter can prove it created and owns that exact migration artifact.

**Acceptance check**

- With `/etc/tmpfiles.d/trcc-rapl.conf` present, `thermalwriter setup-udev` should not remove it.
- The command may print a warning explaining possible RAPL permission conflicts.

**Repository resolution**

- Removed the automatic `remove_file(STALE_TRCC_TMPFILE)` path from `thermalwriter setup-udev`.
- The command now warns if `/etc/tmpfiles.d/trcc-rapl.conf` exists and leaves remediation to the user.
- `cargo test cli::tests` passed; grep confirmed no `remove_file(STALE_TRCC_TMPFILE)` path remains.

---

## P3 Medium Priority

### 9. `SECURITY.md` does not provide a reliable private reporting channel

**Status:** resolved in repository.

**Evidence**

- `SECURITY.md:7` tells reporters to email the address on the `mgaruccio` GitHub profile or open a private advisory if enabled.
- The public GitHub profile page did not expose an email address at review time.

**Impact**

External reporters may have no reliable private path for USB, D-Bus, child-process, or udev vulnerabilities.

**Recommended fix**

- Add a concrete security contact address.
- Or enable GitHub private vulnerability reporting and link directly to it.
- Avoid conditional language unless the private advisory path is actually enabled.

**Acceptance check**

A reporter can open `SECURITY.md` and immediately identify one working private contact path.

**Repository resolution**

- `SECURITY.md` now gives direct private reporting paths: the repository's GitHub private vulnerability report URL and the maintainer's Mastodon direct-message handle found on the public GitHub profile.

---

### 10. Release tarball README points at docs/assets that are not staged into the tarball

**Status:** resolved in repository.

**Evidence**

- README embeds a preview image:
  - `README.md:17` — `docs/assets/neon-dash-v2-preview.png`.
- README links project docs and skill guide:
  - `README.md:154-160`.
- Release workflow stages only daemon binary, README, LICENSE, installer scripts, RAPL rule, and systemd unit:
  - `.github/workflows/release.yml:31-42`.

**Impact**

Runtime is not affected because layouts, wrappers, and backgrounds are embedded in the binary and seeded on daemon start:

- `src/config.rs:387-405` uses `include_str!` / `include_bytes!` for layouts, wrappers, and backgrounds.

However, users browsing the extracted release tarball see README image/doc links that do not exist locally.

**Recommended fix**

Either:

1. Include linked docs/assets in the daemon tarball, or
2. Adjust release-tarball README links to absolute GitHub URLs.

If including local docs, stage at least:

- `CHANGELOG.md`
- `CONTRIBUTING.md`
- `SECURITY.md`
- `docs/configuration.md`
- `docs/gui.md`
- `docs/release.md`
- `docs/architecture.md`
- `docs/assets/neon-dash-v2-preview.png`

**Acceptance check**

Extract the release tarball and verify all README relative links resolve locally, or intentionally point to GitHub.

**Repository resolution**

- Release workflow now stages README-linked docs, `docs/assets/neon-dash-v2-preview.png`, top-level project docs, and `skills/designing-layouts/SKILL.md` into the daemon tarball.
- Verified every newly staged path exists in the repository.

---

### 11. Crates.io/package README links to a skill file not included in the crate package

**Status:** resolved in repository.

**Evidence**

- README links `skills/designing-layouts/SKILL.md`:
  - `README.md:160`.
- `Cargo.toml` package include list omits `/skills/**`:
  - `Cargo.toml:13-42`.
- `cargo package --list -p thermalwriter` confirmed no `skills/` entry.

**Impact**

The link works on GitHub, but is broken from the packaged/crates.io README surface.

**Recommended fix**

- Move the layout-design guide under `docs/`, or
- Add `/skills/designing-layouts/SKILL.md` and any required referenced files to `Cargo.toml` `include`.

**Acceptance check**

`cargo package --list -p thermalwriter` includes every README-linked relative file.

**Repository resolution**

- Added `/skills/designing-layouts/SKILL.md` to `Cargo.toml` `package.include`.
- `cargo package --list -p thermalwriter --allow-dirty` now lists `skills/designing-layouts/SKILL.md`.

---

### 12. Public repo contains internal planning/transcript artifacts

**Status:** resolved in repository.

**Evidence**

- `docs/release.md:18-19` says the crate package should not include old implementation plans under `docs/plans/`; package output correctly excludes them.
- The GitHub repository still contains `docs/plans/` and `docs/brainstorms/`.
- Examples of public internal/process language:
  - `docs/plans/2026-03-23-display-benchmark-impl.md:3` — "For Claude".
  - `docs/plans/2026-03-23-display-benchmark-impl.md:97` — "Wait for user response".
  - `docs/brainstorms/2026-05-29-gui-streaming-conky-design.md:11-12` — research/review agents and Gemini references.
  - `docs/plans/2026-05-08-lcd-background-images-design.md:40` — mentions an API key existing in environment, without exposing the key.

**Impact**

No secret value was found, but these files look like internal execution plans rather than public project documentation. They will distract contributors and reduce polish/trust during public launch.

**Recommended fix**

- Move internal plans/brainstorms out of the public repo before advertising, or
- Archive them under a clearly labeled `docs/internal-history/` if public history is intentional.
- Remove private-environment references and agent-execution instructions.

**Acceptance check**

A fresh public visitor should not encounter internal agent instructions, approval checkpoints, or private environment references while browsing docs.

**Repository resolution**

- Removed `docs/plans/` and `docs/brainstorms/` from the public repository tree instead of archiving internal execution artifacts under public docs.
- Verified both paths are absent after removal.

---

### 13. GUI docs claim GIF import support, but GUI allows only PNG/JPEG extensions

**Status:** resolved in repository.

**Evidence**

- `docs/gui.md:34` says background imports handle PNG, JPEG, and GIF-capable image formats.
- GUI background extension allowlist is only PNG/JPG/JPEG:
  - `gui/src-tauri/src/commands.rs:653-657`.
  - `gui/src-tauri/src/commands.rs:680-683`.

**Impact**

Users trying to import `.gif` backgrounds through the GUI will receive an unsupported extension error.

**Recommended fix**

Either:

- Add `.gif` support to GUI import/list validation, or
- Change docs to say PNG/JPEG only.

**Acceptance check**

Docs and GUI validation agree.

**Repository resolution**

- Updated `docs/gui.md` to say the background gallery imports PNG and JPEG images only, matching the GUI extension allowlist.

---

## P4 Low Priority

### 14. Stale internal comments remain in runtime GUI command source

**Status:** resolved in repository.

**Evidence**

- `gui/src-tauri/src/commands.rs:374` contains `dev-2: check`.
- `gui/src-tauri/src/commands.rs:502` contains `dev-2's Stream tab`.

**Impact**

No runtime impact. This is visible source polish before public sharing.

**Recommended fix**

Replace with neutral maintainer-facing comments.

**Repository resolution**

- Replaced the two `dev-2` comments in `gui/src-tauri/src/commands.rs` with neutral maintainer-facing descriptions.
- Grep confirmed no `dev-2`, `Claude`, or `Gemini` markers remain in that runtime GUI command source.

---

## Existing Strengths

These are good signs for public readiness once the blockers above are fixed:

- Rust workspace tests pass.
- No-default-features tests pass.
- Clippy passes with warnings denied.
- GUI production build passes.
- `cargo package -p thermalwriter` succeeds and excludes old plans/brainstorms from the crate package.
- Built-in runtime layouts, wrappers, and backgrounds are embedded in the daemon binary and seeded on startup.
- Path traversal validation exists for layouts/backgrounds:
  - daemon: `src/service/dbus.rs:152-190`.
  - GUI: `gui/src-tauri/src/commands.rs:725-779`.
- Stream preview frame dumping refuses shared `/tmp` and uses `$XDG_RUNTIME_DIR`:
  - `src/service/frame_dump.rs:1-23`.
- Release workflow builds daemon tarball and GUI AppImage/deb artifacts:
  - `.github/workflows/release.yml:28-73`.

## Fix Completion Summary

1. USB udev rule for `87ad:70db`: resolved in repository.
2. Negotiated dimensions in runtime mode changes: resolved in repository; non-480 hotplug after absent startup is explicitly narrowed in README.
3. Daemon startup without hardware: resolved in repository.
4. Xvfb auth/frame directory privacy: resolved in repository.
5. D-Bus command-exec trust boundary: resolved in repository by rejecting the shell-string public path and documenting structured argv streaming.
6. `CARGO_HOME` vs systemd `ExecStart`: resolved in repository.
7. Source build native dependency docs: resolved in repository.
8. `trcc` tmpfiles ownership: resolved in repository.
9. Security reporting contact: resolved in repository.
10. Release tarball README links: resolved in repository.
11. Crates.io/package README skill link: resolved in repository.
12. Internal planning docs: resolved in repository.
13. GIF background documentation mismatch: resolved in repository.
14. Stale GUI command comments: resolved in repository.

## Public Share Guidance

### Broad public announcement

Repository-side P1/P2 blockers are fixed and attached-hardware smoke checks passed. Before a broad announcement, run the clean-host acceptance checks that could not be performed in this repository checkout:

- Clean user install can access the USB cooler without sudo after udev reload/replug.
- Service stays running when hardware is absent and after physical detach/reattach.
- `thermalwriter ctl status` works before and after physical device attach and reports `connected` correctly.
- Generated user unit points to the installed binary when `CARGO_HOME` is customized during a real install.

### Small private beta

Still acceptable with the normal beta caveats:

- Tested hardware: Thermalright Peerless Vision / GrandVision 360 AIO, USB `87ad:70db`.
- Linux/systemd only.
- Other device dimensions are experimental when the device is connected before daemon startup; absent-at-start hotplug currently targets the known 480x480 path.
- Xvfb streaming is experimental and can run local commands as the user through validated structured argv.
