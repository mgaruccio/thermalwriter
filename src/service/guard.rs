//! Service and device ownership guard for active `validate-device` runs.
//!
//! [`ServiceGuard`] snapshots whether the thermalwriter user systemd unit was
//! active, stops it when needed, and verifies the selected device is not still
//! held before validation performs any init/output I/O. On exit it restarts the
//! unit only when it was active before acquisition — never enabling units or
//! changing persistence.
//!
//! Passive validation (`--passive`) performs no active I/O and does not use this
//! guard.

use anyhow::{Context, Result, bail, ensure};
use log::{error, warn};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::transport::hid_report::UsbBusAddress;

/// Default user-systemd unit for the thermalwriter daemon.
pub const DEFAULT_SERVICE_UNIT: &str = "thermalwriter.service";

/// Session D-Bus well-known name owned by the running daemon.
pub const DAEMON_DBUS_NAME: &str = "com.thermalwriter.Service";

/// Control plane for a single user-systemd unit (stop/start/is-active only).
pub trait ServiceControl {
    fn is_active(&self, unit: &str) -> Result<bool>;
    fn stop(&self, unit: &str) -> Result<()>;
    fn start(&self, unit: &str) -> Result<()>;
}

/// Detect whether the daemon or another process still holds the validation target.
pub trait DeviceOwnership {
    /// Returns `true` when the target device or daemon name is still concurrently owned.
    fn is_concurrently_owned(&self, target: &OwnershipTarget) -> Result<bool>;
}

/// Selected USB/hidraw node used for exclusive-ownership verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipTarget {
    usb: Option<UsbBusAddress>,
    hidraw_devnode: Option<PathBuf>,
}

impl OwnershipTarget {
    pub fn hidraw(name: &str) -> Result<Self> {
        ensure!(
            !name.is_empty() && !name.contains('/') && name.starts_with("hidraw"),
            "invalid hidraw name {name:?}"
        );
        Ok(Self {
            usb: None,
            hidraw_devnode: Some(PathBuf::from(format!("/dev/{name}"))),
        })
    }

    pub fn usb_bus_address(bus: u8, address: u8) -> Self {
        Self {
            usb: Some(UsbBusAddress { bus, address }),
            hidraw_devnode: None,
        }
    }

    pub fn with_usb_bus_address(mut self, bus: u8, address: u8) -> Self {
        self.usb = Some(UsbBusAddress { bus, address });
        self
    }

    pub fn with_hidraw_devnode(mut self, devnode: impl Into<PathBuf>) -> Self {
        self.hidraw_devnode = Some(devnode.into());
        self
    }

    pub(crate) fn usb_devnode(&self) -> Option<PathBuf> {
        self.usb
            .map(|usb| PathBuf::from(format!("/dev/bus/usb/{:03}/{:03}", usb.bus, usb.address)))
    }

    pub(crate) fn hidraw_devnode(&self) -> Option<&Path> {
        self.hidraw_devnode.as_deref()
    }
}

/// Real user-systemd control via `systemctl --user` (never enable/disable).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemdUserControl;

impl ServiceControl for SystemdUserControl {
    fn is_active(&self, unit: &str) -> Result<bool> {
        let output = Command::new("systemctl")
            .args(["--user", "is-active", "--quiet", unit])
            .output()
            .with_context(|| format!("run systemctl is-active for {unit}"))?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(3) => Ok(false),
            Some(4) => bail!("systemd unit {unit} not found"),
            _ => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!(
                    "systemctl is-active for {unit} failed (exit {:?}): {stderr}",
                    output.status.code()
                );
            }
        }
    }

    fn stop(&self, unit: &str) -> Result<()> {
        run_systemctl_user(&["stop", unit], unit, "stop")
    }

    fn start(&self, unit: &str) -> Result<()> {
        run_systemctl_user(&["start", unit], unit, "start")
    }
}

fn run_systemctl_user(args: &[&str], unit: &str, verb: &str) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .with_context(|| format!("run systemctl --user {verb} {unit}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "systemctl --user {verb} {unit} failed (exit {:?}): {stderr}",
            output.status.code()
        );
    }
}

/// Ownership probe: session D-Bus daemon name plus exact `/proc/*/fd` devnode scan.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultDeviceOwnership;

impl DeviceOwnership for DefaultDeviceOwnership {
    fn is_concurrently_owned(&self, target: &OwnershipTarget) -> Result<bool> {
        if dbus_daemon_owned()? {
            return Ok(true);
        }
        let mut devnodes = Vec::new();
        if let Some(hidraw) = target.hidraw_devnode() {
            devnodes.push(hidraw.to_path_buf());
        }
        if let Some(usb) = target.usb_devnode() {
            devnodes.push(usb);
        }
        for devnode in &devnodes {
            if devnode_open_elsewhere(devnode)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn dbus_daemon_owned() -> Result<bool> {
    let output = Command::new("busctl")
        .args(["--user", "status", DAEMON_DBUS_NAME])
        .output()
        .with_context(|| format!("run busctl status {DAEMON_DBUS_NAME}"))?;
    if output.status.success() {
        return Ok(true);
    }
    // Name not owned is the common success path after stop; only treat hard failures as errors.
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "busctl status {DAEMON_DBUS_NAME} failed (exit {:?}): {stderr}",
        output.status.code()
    );
}

fn devnode_open_elsewhere(devnode: &Path) -> Result<bool> {
    let canonical = std::fs::canonicalize(devnode)
        .with_context(|| format!("canonicalize device node {}", devnode.display()))?;
    for entry in std::fs::read_dir("/proc").with_context(|| "read /proc")? {
        let entry = entry?;
        let pid_name = entry.file_name();
        let pid = match pid_name.to_string_lossy().parse::<u32>() {
            Ok(pid) => pid,
            Err(_) => continue,
        };
        let fd_dir = entry.path().join("fd");
        let fds = match std::fs::read_dir(&fd_dir) {
            Ok(fds) => fds,
            Err(_) => continue,
        };
        for fd in fds.flatten() {
            let link = match std::fs::read_link(fd.path()) {
                Ok(link) => link,
                Err(_) => continue,
            };
            if link == canonical || link == devnode {
                warn!(
                    "device node {} still open by pid {pid} (fd {})",
                    devnode.display(),
                    fd.file_name().to_string_lossy()
                );
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// RAII guard that stops the daemon when needed and restores prior unit state on drop.
pub struct ServiceGuard<C: ServiceControl, O: DeviceOwnership> {
    control: C,
    #[allow(dead_code)]
    ownership: O,
    unit: String,
    was_active: bool,
    restored: bool,
    restore_error: Option<String>,
}

impl<C: ServiceControl, O: DeviceOwnership> ServiceGuard<C, O> {
    /// Snapshot unit activity, stop when active, and verify exclusive device ownership.
    pub fn acquire(
        control: C,
        ownership: O,
        target: &OwnershipTarget,
        unit: impl Into<String>,
    ) -> Result<Self> {
        let unit = unit.into();
        let was_active = control
            .is_active(&unit)
            .with_context(|| format!("query active state for {unit}"))?;
        if was_active {
            control
                .stop(&unit)
                .with_context(|| format!("stop active unit {unit}"))?;
        }
        if ownership.is_concurrently_owned(target)? {
            bail!(
                "device or daemon still concurrently owned after stopping {unit}; \
                 refusing active validation I/O"
            );
        }
        Ok(Self {
            control,
            ownership,
            unit,
            was_active,
            restored: false,
            restore_error: None,
        })
    }

    pub fn was_active(&self) -> bool {
        self.was_active
    }

    pub fn restored(&self) -> bool {
        self.restored
    }

    pub fn restore_error(&self) -> Option<&str> {
        self.restore_error.as_deref()
    }

    /// Restart the unit only when it was active before acquisition.
    pub fn restore(&mut self) -> Result<()> {
        if self.restored {
            return match self.restore_error.as_deref() {
                Some(msg) => Err(anyhow::anyhow!("{msg}")),
                None => Ok(()),
            };
        }
        let result = if self.was_active {
            self.control
                .start(&self.unit)
                .with_context(|| format!("restart unit {}", self.unit))
        } else {
            Ok(())
        };
        if let Err(err) = &result {
            self.restore_error = Some(format!("{err:#}"));
        }
        self.restored = true;
        result
    }
}

impl<C: ServiceControl, O: DeviceOwnership> Drop for ServiceGuard<C, O> {
    fn drop(&mut self) {
        if self.restored {
            return;
        }
        if let Err(err) = self.restore() {
            error!("ServiceGuard drop failed to restore {}: {err:#}", self.unit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct FakeControl {
        active: bool,
        stop_fail: bool,
        start_fail: bool,
        calls: Rc<RefCell<Vec<&'static str>>>,
    }

    impl Default for FakeControl {
        fn default() -> Self {
            Self {
                active: false,
                stop_fail: false,
                start_fail: false,
                calls: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl FakeControl {
        fn record(&self, call: &'static str) {
            self.calls.borrow_mut().push(call);
        }
    }

    impl ServiceControl for FakeControl {
        fn is_active(&self, _unit: &str) -> Result<bool> {
            self.record("is_active");
            Ok(self.active)
        }

        fn stop(&self, _unit: &str) -> Result<()> {
            self.record("stop");
            if self.stop_fail {
                Err(anyhow::anyhow!("stop failed"))
            } else {
                Ok(())
            }
        }

        fn start(&self, _unit: &str) -> Result<()> {
            self.record("start");
            if self.start_fail {
                Err(anyhow::anyhow!("start failed"))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct FakeOwnership {
        owned: bool,
        checks: Rc<RefCell<u32>>,
    }

    impl DeviceOwnership for FakeOwnership {
        fn is_concurrently_owned(&self, _target: &OwnershipTarget) -> Result<bool> {
            *self.checks.borrow_mut() += 1;
            Ok(self.owned)
        }
    }

    fn sample_target() -> OwnershipTarget {
        OwnershipTarget::hidraw("hidraw0").expect("hidraw target")
    }

    #[test]
    fn inactive_unit_skips_stop_and_restore_is_noop() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let control = FakeControl {
            active: false,
            calls: Rc::clone(&calls),
            ..Default::default()
        };
        let ownership = FakeOwnership::default();
        let mut guard = ServiceGuard::acquire(
            control,
            ownership,
            &sample_target(),
            "thermalwriter.service",
        )
        .expect("acquire when inactive");
        assert!(!guard.was_active());
        guard.restore().expect("restore noop");
        assert!(guard.restored());
        let recorded = calls.borrow();
        assert_eq!(recorded.as_slice(), &["is_active"]);
    }

    #[test]
    fn active_unit_stops_and_restore_starts() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let control = FakeControl {
            active: true,
            calls: Rc::clone(&calls),
            ..Default::default()
        };
        let ownership = FakeOwnership::default();
        let mut guard = ServiceGuard::acquire(
            control,
            ownership,
            &sample_target(),
            "thermalwriter.service",
        )
        .expect("acquire when active");
        assert!(guard.was_active());
        guard.restore().expect("restore starts unit");
        let recorded = calls.borrow();
        assert_eq!(recorded.as_slice(), &["is_active", "stop", "start"]);
    }

    #[test]
    fn stop_failure_errors_acquire() {
        let control = FakeControl {
            active: true,
            stop_fail: true,
            ..Default::default()
        };
        let ownership = FakeOwnership::default();
        let err = match ServiceGuard::acquire(
            control,
            ownership,
            &sample_target(),
            "thermalwriter.service",
        ) {
            Ok(_) => panic!("stop failure must abort acquire"),
            Err(err) => err,
        };
        assert!(format!("{err:#}").contains("stop failed"), "{err:#}");
    }

    #[test]
    fn ownership_retained_errors_acquire() {
        let control = FakeControl {
            active: true,
            ..Default::default()
        };
        let ownership = FakeOwnership {
            owned: true,
            ..Default::default()
        };
        let err = match ServiceGuard::acquire(
            control,
            ownership,
            &sample_target(),
            "thermalwriter.service",
        ) {
            Ok(_) => panic!("retained ownership must abort acquire"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("concurrently owned"), "{err}");
    }

    #[test]
    fn success_path_is_exclusive() {
        let checks = Rc::new(RefCell::new(0));
        let ownership = FakeOwnership {
            owned: false,
            checks: Rc::clone(&checks),
        };
        let guard = ServiceGuard::acquire(
            FakeControl::default(),
            ownership,
            &sample_target(),
            "thermalwriter.service",
        )
        .expect("exclusive acquire");
        assert!(!guard.was_active());
        assert_eq!(*checks.borrow(), 1);
    }

    #[test]
    fn restore_failure_is_returned_and_recorded() {
        let control = FakeControl {
            active: true,
            start_fail: true,
            ..Default::default()
        };
        let ownership = FakeOwnership::default();
        let mut guard = ServiceGuard::acquire(
            control,
            ownership,
            &sample_target(),
            "thermalwriter.service",
        )
        .expect("acquire");
        let err = guard
            .restore()
            .expect_err("restore must surface start failure");
        assert!(format!("{err:#}").contains("start failed"), "{err:#}");
        assert!(guard.restored());
        assert!(guard.restore_error().is_some());
    }

    #[test]
    fn drop_restores_when_not_explicitly_restored() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let control = FakeControl {
            active: true,
            calls: Rc::clone(&calls),
            ..Default::default()
        };
        let ownership = FakeOwnership::default();
        let guard = ServiceGuard::acquire(
            control,
            ownership,
            &sample_target(),
            "thermalwriter.service",
        )
        .expect("acquire");
        drop(guard);
        let recorded = calls.borrow();
        assert_eq!(recorded.as_slice(), &["is_active", "stop", "start"]);
    }

    #[test]
    fn systemd_user_control_never_calls_enable() {
        let src = include_str!("guard.rs");
        assert!(src.contains("is-active"), "must query active state");
        assert!(src.contains("\"stop\""), "must support stop");
        assert!(src.contains("\"start\""), "must support start");
        assert!(
            !src.contains("\"enable\"") && !src.contains("\"disable\""),
            "must not enable or disable units"
        );
    }

    #[test]
    fn abort_path_still_attempts_restore_on_drop() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let control = FakeControl {
            active: true,
            start_fail: true,
            calls: Rc::clone(&calls),
            ..Default::default()
        };
        let ownership = FakeOwnership::default();
        let guard = ServiceGuard::acquire(
            control,
            ownership,
            &sample_target(),
            "thermalwriter.service",
        )
        .expect("acquire");
        drop(guard);
        let recorded = calls.borrow();
        assert!(
            recorded.ends_with(&["start"]),
            "drop must attempt restore: {recorded:?}"
        );
    }

    #[test]
    fn ownership_target_usb_devnode_path() {
        let target = OwnershipTarget::usb_bus_address(2, 7);
        assert_eq!(
            target.usb_devnode(),
            Some(PathBuf::from("/dev/bus/usb/002/007"))
        );
    }
}
