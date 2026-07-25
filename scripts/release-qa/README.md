# Release QA harness (#90)

Clean-machine install validation for thermalwriter release artifacts.

## Layers

| Layer | What | Command |
| --- | --- | --- |
| **L0** | Checksums, tarball layout, README relative links | `host/run-l0.sh v0.1.1` |
| **L1** | Ubuntu tarball + GUI packages; Arch source + AppImage | `host/run-l1.sh v0.1.1` |
| **L2** | Host cooler unplug/replug (`connected` transitions) | `host/hw-attach-smoke.sh` |

L1 needs KVM (`/dev/kvm`), `qemu-system-x86_64`, `qemu-img`, and `cloud-localds` (package `cloud-image-utils`).

## Quick start

```sh
# Host deps (Arch example)
sudo pacman -S --needed qemu-desktop edk2-ovmf cloud-image-utils cdrtools curl

# Static checks only (no VMs)
./scripts/release-qa/host/run-l0.sh v0.1.1

# Full clean-VM matrix (downloads cloud images on first run)
./scripts/release-qa/host/run-all.sh v0.1.1

# Ubuntu only / reset disks
./scripts/release-qa/host/run-l1.sh v0.1.1 --ubuntu-only --reset-vms
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
| Detach/reattach `connected` | `host/hw-attach-smoke.sh` (L2, bare metal) |

## Notes

- Guests enable `loginctl linger` so `systemctl --user` works over SSH.
- Source installs pin the **release tag**, not `main`.
- GUI smoke = package installs + short `xvfb-run` launch (not full UI click-through).
- Hardware USB passthrough into QEMU is intentionally out of L1; use L2 on the host with the Peerless Vision.

## Known v0.1.1 L0 finding

Published tarball omits `docs/comparison-methodology.md` (README still links it).
`release.yml` is patched so the next tag stages that file. Install paths (L1) are unaffected.
