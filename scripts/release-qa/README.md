# Release QA harness (#90)

Clean-machine install validation for thermalwriter release artifacts.

## Layers

| Layer | What | Command |
| --- | --- | --- |
| **L0** | Checksums, tarball layout (incl. tray), README relative links | `host/run-l0.sh v0.1.2` |
| **L1** | Ubuntu tarball + GUI packages; Arch source + AppImage | `host/run-l1.sh v0.1.2` |
| **Tray** | SNI registration on Ubuntu GNOME AppIndicator + Arch KDE/kded | `host/run-tray-desktop.sh v0.1.2` |
| **L2** | Host cooler unplug/replug (`connected` transitions) | `host/hw-attach-smoke.sh` |

L1 needs KVM (`/dev/kvm`), `qemu-system-x86_64`, `qemu-img`, and `cloud-localds` (package `cloud-image-utils`).

## Quick start

```sh
# Host deps (Arch example)
sudo pacman -S --needed qemu-desktop edk2-ovmf cloud-image-utils cdrtools curl

# Static checks only (no VMs)
./scripts/release-qa/host/run-l0.sh v0.1.2

# Full clean-VM matrix (downloads cloud images on first run)
./scripts/release-qa/host/run-all.sh v0.1.2

# Tray SNI smoke (needs Ubuntu + Arch QA VMs up; GNOME AppIndicator / kded packages)
./scripts/release-qa/host/run-tray-desktop.sh --local-bin ./target/release/thermalwriter-tray
# or against a published tag:
# ./scripts/release-qa/host/run-tray-desktop.sh v0.1.2

# Ubuntu only / reset disks
./scripts/release-qa/host/run-l1.sh v0.1.2 --ubuntu-only --reset-vms
```

Artifacts cache: `${XDG_CACHE_HOME:-~/.cache}/thermalwriter-qa/artifacts/<tag>/`  
VM disks: `~/vms/thermalwriter-qa/`  
Reports: `scripts/release-qa/out/<tag>/`

## SSH

Guests use user `qa` with your default SSH public key (`~/.ssh/id_ed25519.pub`).  
Ubuntu: `ssh -p 2222 qa@127.0.0.1`  
Arch: `ssh -p 2223 qa@127.0.0.1`

## Environment overrides

| Variable | Default | Purpose |
| --- | --- | --- |
| `THERMALWRITER_QA_CACHE` | `~/.cache/thermalwriter-qa` | Artifact cache root |
| `THERMALWRITER_QA_VM_DIR` | `~/vms/thermalwriter-qa` | VM disk/seed root |
| `THERMALWRITER_QA_SSH_PUBKEY` | `~/.ssh/id_ed25519.pub` | Injected into cloud-init |
| `THERMALWRITER_QA_SSH_KEY` | matching private key | Host→guest SSH |
| `THERMALWRITER_QA_UBUNTU_SSH_PORT` | `2222` | Host forward port |
| `THERMALWRITER_QA_ARCH_SSH_PORT` | `2223` | Host forward port |
| `THERMALWRITER_QA_FORCE_FETCH` | `0` | Re-download artifacts |
| `THERMALWRITER_QA_REPO` | `mgaruccio/thermalwriter` | `gh release download` repo |

## Mapping to #90

| Checklist item | Script |
| --- | --- |
| Ubuntu LTS tarball `install.sh` | `guest/ubuntu-tarball.sh` |
| Arch source `install.sh` | `guest/arch-source.sh` |
| Daemon up / D-Bus with no hardware | both guests |
| Custom `CARGO_HOME` → unit `ExecStart` | `guest/ubuntu-tarball.sh` |
| GUI `.deb` + AppImage | `guest/ubuntu-gui.sh` (+ AppImage on Arch) |
| Tarball README relative links | `check-artifacts.sh` (L0) |
| Detach/reattach `connected` | optional `host/hw-attach-smoke.sh` (L2) — **not required to close #90** |

## Acceptance for #90

**Done when L0 + L1 pass across the multi-distro matrix** (Ubuntu LTS tarball/GUI + Arch source/AppImage), including daemon/D-Bus with no hardware and custom `CARGO_HOME` → unit `ExecStart`.

L2 physical unplug/replug is optional tooling for host debugging. Install-path confidence comes from different distros (and GUI smoke under xvfb), not from USB cable cycles or DE matrix coverage.

## Notes

- Guests enable `loginctl linger` so `systemctl --user` works over SSH.
- Source installs pin the **release tag**, not `main`.
- GUI smoke = package installs + short `xvfb-run` launch (not full UI click-through).
- Hardware USB passthrough into QEMU is intentionally out of scope; host already runs the cooler day-to-day.

## Known v0.1.1 L0 finding

Published tarball omits `docs/comparison-methodology.md` (README still links it).
`release.yml` is patched so the next tag stages that file. Install paths (L1) are unaffected.
