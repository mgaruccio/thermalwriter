// SPDX-License-Identifier: GPL-3.0-or-later
//
// Shared libusb helpers used by the USB transport backends.

use anyhow::{Context, Result, bail};
use rusb::GlobalContext;

/// Locate a USB device by bus number and address on the default libusb context.
pub fn find_device(bus: u8, address: u8) -> Result<rusb::Device<GlobalContext>> {
    let list = rusb::devices().context("libusb device list failed")?;
    for device in list.iter() {
        if device.bus_number() == bus && device.address() == address {
            return Ok(device);
        }
    }
    bail!("no USB device at bus={bus} address={address}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_device_missing_bus_address_errors() {
        // Bus 255 / address 255 is vanishingly unlikely to exist; the helper
        // must return a structured error rather than panicking.
        let err = find_device(255, 255).expect_err("missing device must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("bus=255") && msg.contains("address=255"),
            "unexpected error: {msg}"
        );
    }
}
