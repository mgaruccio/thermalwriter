// SPDX-License-Identifier: GPL-3.0-or-later
//
// Shareable USB descriptor inventory and pure route derivation helpers.

use serde::{Deserialize, Serialize};

#[cfg(feature = "daemon")]
use anyhow::{Context, Result};
#[cfg(feature = "daemon")]
use rusb::{Direction, TransferType};

/// USB endpoint transfer type for shareable inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsbTransferKind {
    Bulk,
    Interrupt,
    Isochronous,
}

/// Endpoint direction for shareable inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsbDirection {
    In,
    Out,
}

#[cfg(feature = "daemon")]
impl From<Direction> for UsbDirection {
    fn from(direction: Direction) -> Self {
        match direction {
            Direction::In => Self::In,
            Direction::Out => Self::Out,
        }
    }
}

/// One endpoint capability from the active configuration descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbEndpointCapability {
    pub address: u8,
    pub direction: UsbDirection,
    pub transfer: UsbTransferKind,
    pub max_packet_size: u16,
    pub interval: u8,
}

/// One interface alternate setting and its endpoint list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbInterfaceShape {
    pub number: u8,
    pub alternate_setting: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<UsbEndpointCapability>,
}

/// Shareable USB identity: VID/PID, firmware BCD, and interface topology.
///
/// Bus number and device address are intentionally excluded; they are per-run
/// selectors used only while opening a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbFingerprint {
    pub vid: u16,
    pub pid: u16,
    pub bcd_device: String,
    pub interfaces: Vec<UsbInterfaceShape>,
}

/// Per-run USB identity used during discovery/open. Not shareable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbRunIdentity {
    pub bus: u8,
    pub address: u8,
    pub fingerprint: UsbFingerprint,
}

/// Bulk IN/OUT pair derived from endpoint inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedBulkPair {
    pub interface: u8,
    pub ep_in: u8,
    pub ep_out: u8,
    pub vendor_class: bool,
}

/// HID interrupt-IN endpoint observed on a HID-class interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidInterruptIn {
    pub interface: u8,
    pub address: u8,
    pub max_packet_size: u16,
}

const USB_CLASS_HID: u8 = 3;
const USB_CLASS_VENDOR: u8 = 255;

/// Render `bcdDevice` as `major.minor` (for example `0x0407` → `"4.07"`).
pub fn format_bcd_device(bcd: u16) -> String {
    let major_nibble = ((bcd >> 12) & 0xF) as u8;
    let major_digit = ((bcd >> 8) & 0xF) as u8;
    let minor_tens = ((bcd >> 4) & 0xF) as u8;
    let minor_ones = (bcd & 0xF) as u8;
    let major = major_nibble * 10 + major_digit;
    let minor = minor_tens * 10 + minor_ones;
    format!("{major}.{minor:02}")
}

#[cfg(feature = "daemon")]
/// Render a parsed libusb BCD version for shareable inventory.
pub fn format_bcd_version(version: rusb::Version) -> String {
    format!(
        "{}.{:02}",
        version.major(),
        version.minor() * 10 + version.sub_minor()
    )
}

#[cfg(feature = "daemon")]
fn transfer_kind(transfer: TransferType) -> Option<UsbTransferKind> {
    match transfer {
        TransferType::Bulk => Some(UsbTransferKind::Bulk),
        TransferType::Interrupt => Some(UsbTransferKind::Interrupt),
        TransferType::Isochronous => Some(UsbTransferKind::Isochronous),
        TransferType::Control => None,
    }
}

#[cfg(feature = "daemon")]
fn interface_shape_from_descriptor(
    number: u8,
    desc: &rusb::InterfaceDescriptor,
) -> UsbInterfaceShape {
    let endpoints = desc
        .endpoint_descriptors()
        .filter_map(|ep| {
            let transfer = transfer_kind(ep.transfer_type())?;
            Some(UsbEndpointCapability {
                address: ep.address(),
                direction: ep.direction().into(),
                transfer,
                max_packet_size: ep.max_packet_size(),
                interval: ep.interval(),
            })
        })
        .collect();
    UsbInterfaceShape {
        number,
        alternate_setting: desc.setting_number(),
        class: desc.class_code(),
        subclass: desc.sub_class_code(),
        protocol: desc.protocol_code(),
        endpoints,
    }
}

/// Build shareable inventory from a libusb device handle.
#[cfg(feature = "daemon")]
pub fn fingerprint_from_device<T: rusb::UsbContext>(
    device: &rusb::Device<T>,
) -> Result<UsbFingerprint> {
    let desc = device
        .device_descriptor()
        .context("failed to read USB device descriptor")?;
    let config = device
        .active_config_descriptor()
        .context("active config descriptor")?;
    let mut interfaces = Vec::new();
    for iface in config.interfaces() {
        for desc in iface.descriptors() {
            interfaces.push(interface_shape_from_descriptor(iface.number(), &desc));
        }
    }
    Ok(UsbFingerprint {
        vid: desc.vendor_id(),
        pid: desc.product_id(),
        bcd_device: format_bcd_version(desc.device_version()),
        interfaces,
    })
}

fn bulk_pair_on_interface(shape: &UsbInterfaceShape) -> Option<(u8, u8, bool)> {
    let mut ep_in = 0u8;
    let mut ep_out = 0u8;
    for endpoint in &shape.endpoints {
        if endpoint.transfer != UsbTransferKind::Bulk {
            continue;
        }
        match endpoint.direction {
            UsbDirection::In => ep_in = endpoint.address,
            UsbDirection::Out => ep_out = endpoint.address,
        }
    }
    if ep_in != 0 && ep_out != 0 {
        let vendor = shape.class == USB_CLASS_VENDOR || shape.class == 0;
        Some((ep_in, ep_out, vendor))
    } else {
        None
    }
}

fn best_bulk_pair(fingerprint: &UsbFingerprint, vendor_only: bool) -> Option<DerivedBulkPair> {
    let mut best: Option<DerivedBulkPair> = None;
    for shape in &fingerprint.interfaces {
        let Some((ep_in, ep_out, vendor)) = bulk_pair_on_interface(shape) else {
            continue;
        };
        if vendor_only && !vendor {
            continue;
        }
        let candidate = DerivedBulkPair {
            interface: shape.number,
            ep_in,
            ep_out,
            vendor_class: vendor,
        };
        match best {
            None => best = Some(candidate),
            Some(current) if vendor && !current.vendor_class => best = Some(candidate),
            _ => {}
        }
    }
    best
}

/// Derive a vendor-class bulk IN/OUT pair (class 255 or 0).
pub fn derive_vendor_bulk_pair(fingerprint: &UsbFingerprint) -> Option<DerivedBulkPair> {
    best_bulk_pair(fingerprint, true)
}

/// Derive any bulk IN/OUT pair, preferring vendor-class interfaces.
pub fn derive_bulk_pair(fingerprint: &UsbFingerprint) -> Option<DerivedBulkPair> {
    best_bulk_pair(fingerprint, false)
}

/// List HID-class interrupt-IN endpoints. An interface with only interrupt IN
/// is valid inventory; interrupt OUT is not required or invented.
pub fn hid_interrupt_in_endpoints(fingerprint: &UsbFingerprint) -> Vec<HidInterruptIn> {
    fingerprint
        .interfaces
        .iter()
        .filter(|shape| shape.class == USB_CLASS_HID)
        .flat_map(|shape| {
            shape.endpoints.iter().filter_map(|endpoint| {
                (endpoint.transfer == UsbTransferKind::Interrupt
                    && endpoint.direction == UsbDirection::In)
                    .then_some(HidInterruptIn {
                        interface: shape.number,
                        address: endpoint.address,
                        max_packet_size: endpoint.max_packet_size,
                    })
            })
        })
        .collect()
}

/// HID-class interface with interrupt IN and optional interrupt OUT.
///
/// Used by discovery to open Type2 panels that speak HID reports (no bulk pair).
/// When interrupt OUT is absent, `ep_out` is 0 (open path may use SET_REPORT).
pub fn derive_hid_interrupt_pair(fingerprint: &UsbFingerprint) -> Option<(u8, u8, u8)> {
    let mut best: Option<(u8, u8, u8, bool)> = None; // iface, in, out, has_out
    for shape in fingerprint
        .interfaces
        .iter()
        .filter(|shape| shape.class == USB_CLASS_HID)
    {
        let Some(ep_in) = shape.endpoints.iter().find(|endpoint| {
            endpoint.transfer == UsbTransferKind::Interrupt
                && endpoint.direction == UsbDirection::In
        }) else {
            continue;
        };
        let ep_out = shape.endpoints.iter().find(|endpoint| {
            endpoint.transfer == UsbTransferKind::Interrupt
                && endpoint.direction == UsbDirection::Out
        });
        let out_addr = ep_out.map(|e| e.address).unwrap_or(0);
        let has_out = ep_out.is_some();
        match best {
            None => best = Some((shape.number, ep_in.address, out_addr, has_out)),
            Some((_, _, _, false)) if has_out => {
                best = Some((shape.number, ep_in.address, out_addr, has_out));
            }
            _ => {}
        }
    }
    best.map(|(iface, ep_in, ep_out, _)| (iface, ep_in, ep_out))
}

/// Diagnostic for a known VID:PID whose observed shape has no supported route.
pub fn unsupported_known_shape_message(
    vid: u16,
    pid: u16,
    protocol: &str,
    fingerprint: &UsbFingerprint,
) -> String {
    let hid_in = hid_interrupt_in_endpoints(fingerprint);
    let bulk = derive_bulk_pair(fingerprint);
    let mut lines = vec![format!(
        "device {vid:04x}:{pid:04x} bcd_device={} has no supported {protocol} route",
        fingerprint.bcd_device
    )];
    if let Some(pair) = bulk {
        lines.push(format!(
            "observed bulk pair iface={} in=0x{:02x} out=0x{:02x} vendor={}",
            pair.interface, pair.ep_in, pair.ep_out, pair.vendor_class
        ));
    } else {
        lines.push("observed bulk pair: none".to_string());
    }
    if hid_in.is_empty() {
        lines.push("observed HID interrupt IN: none".to_string());
    } else {
        for endpoint in hid_in {
            lines.push(format!(
                "observed HID interrupt IN iface={} addr=0x{:02x} wMaxPacketSize={}",
                endpoint.interface, endpoint.address, endpoint.max_packet_size
            ));
        }
    }
    for shape in &fingerprint.interfaces {
        let endpoint_summary: Vec<String> = shape
            .endpoints
            .iter()
            .map(|ep| {
                format!(
                    "0x{:02x} {:?}/{:?} mps={}",
                    ep.address, ep.transfer, ep.direction, ep.max_packet_size
                )
            })
            .collect();
        lines.push(format!(
            "interface {} alt={} class={} subclass={} protocol={} endpoints=[{}]",
            shape.number,
            shape.alternate_setting,
            shape.class,
            shape.subclass,
            shape.protocol,
            endpoint_summary.join(", ")
        ));
    }
    lines.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(
        address: u8,
        direction: UsbDirection,
        transfer: UsbTransferKind,
        max_packet_size: u16,
    ) -> UsbEndpointCapability {
        UsbEndpointCapability {
            address,
            direction,
            transfer,
            max_packet_size,
            interval: 1,
        }
    }

    fn iface(number: u8, class: u8, endpoints: Vec<UsbEndpointCapability>) -> UsbInterfaceShape {
        UsbInterfaceShape {
            number,
            alternate_setting: 0,
            class,
            subclass: 0,
            protocol: 0,
            endpoints,
        }
    }

    fn fingerprint(interfaces: Vec<UsbInterfaceShape>) -> UsbFingerprint {
        UsbFingerprint {
            vid: 0x0416,
            pid: 0x5302,
            bcd_device: "4.07".to_string(),
            interfaces,
        }
    }

    #[test]
    fn bcd_device_renders_major_minor_string() {
        assert_eq!(format_bcd_device(0x0407), "4.07");
        assert_eq!(format_bcd_device(0x0100), "1.00");
        assert_eq!(format_bcd_device(0x0001), "0.01");
    }

    #[test]
    fn hid_interrupt_in_only_inventory_is_valid() {
        let fp = fingerprint(vec![iface(
            0,
            USB_CLASS_HID,
            vec![endpoint(
                0x81,
                UsbDirection::In,
                UsbTransferKind::Interrupt,
                8,
            )],
        )]);
        let endpoints = hid_interrupt_in_endpoints(&fp);
        assert_eq!(
            endpoints,
            vec![HidInterruptIn {
                interface: 0,
                address: 0x81,
                max_packet_size: 8,
            }]
        );
        assert!(derive_bulk_pair(&fp).is_none());
        assert!(derive_vendor_bulk_pair(&fp).is_none());
    }

    #[test]
    fn derive_true_bulk_pair_from_vendor_interface() {
        let fp = fingerprint(vec![iface(
            1,
            USB_CLASS_VENDOR,
            vec![
                endpoint(0x81, UsbDirection::In, UsbTransferKind::Bulk, 512),
                endpoint(0x02, UsbDirection::Out, UsbTransferKind::Bulk, 512),
            ],
        )]);
        let pair = derive_bulk_pair(&fp).expect("bulk pair");
        assert_eq!(
            pair,
            DerivedBulkPair {
                interface: 1,
                ep_in: 0x81,
                ep_out: 0x02,
                vendor_class: true,
            }
        );
        assert_eq!(derive_vendor_bulk_pair(&fp), Some(pair));
    }

    #[test]
    fn mixed_endpoints_keep_bulk_derivation_separate_from_hid_inventory() {
        let fp = fingerprint(vec![
            iface(
                0,
                USB_CLASS_HID,
                vec![endpoint(
                    0x81,
                    UsbDirection::In,
                    UsbTransferKind::Interrupt,
                    8,
                )],
            ),
            iface(
                1,
                USB_CLASS_VENDOR,
                vec![
                    endpoint(0x82, UsbDirection::In, UsbTransferKind::Bulk, 512),
                    endpoint(0x03, UsbDirection::Out, UsbTransferKind::Bulk, 512),
                    endpoint(0x84, UsbDirection::In, UsbTransferKind::Interrupt, 64),
                ],
            ),
        ]);
        assert_eq!(hid_interrupt_in_endpoints(&fp).len(), 1);
        let pair = derive_bulk_pair(&fp).expect("bulk pair");
        assert_eq!(pair.interface, 1);
        assert_eq!(pair.ep_in, 0x82);
        assert_eq!(pair.ep_out, 0x03);
    }

    #[test]
    fn mass_storage_bulk_is_not_vendor_bulk() {
        let fp = fingerprint(vec![iface(
            0,
            8,
            vec![
                endpoint(0x81, UsbDirection::In, UsbTransferKind::Bulk, 512),
                endpoint(0x02, UsbDirection::Out, UsbTransferKind::Bulk, 512),
            ],
        )]);
        assert!(derive_vendor_bulk_pair(&fp).is_none());
        let pair = derive_bulk_pair(&fp).expect("non-vendor bulk pair");
        assert!(!pair.vendor_class);
    }

    #[test]
    fn dual_shape_prefers_vendor_bulk_over_mass_storage() {
        let fp = fingerprint(vec![
            iface(
                0,
                8,
                vec![
                    endpoint(0x81, UsbDirection::In, UsbTransferKind::Bulk, 512),
                    endpoint(0x02, UsbDirection::Out, UsbTransferKind::Bulk, 512),
                ],
            ),
            iface(
                1,
                USB_CLASS_VENDOR,
                vec![
                    endpoint(0x83, UsbDirection::In, UsbTransferKind::Bulk, 512),
                    endpoint(0x04, UsbDirection::Out, UsbTransferKind::Bulk, 512),
                ],
            ),
        ]);
        let vendor = derive_vendor_bulk_pair(&fp).expect("vendor bulk");
        assert_eq!(vendor.interface, 1);
        assert_eq!(vendor.ep_in, 0x83);
        assert_eq!(vendor.ep_out, 0x04);
    }

    #[test]
    fn unsupported_known_shape_diagnostic_includes_inventory() {
        let fp = fingerprint(vec![iface(
            0,
            USB_CLASS_HID,
            vec![endpoint(
                0x81,
                UsbDirection::In,
                UsbTransferKind::Interrupt,
                8,
            )],
        )]);
        let message = unsupported_known_shape_message(0x0416, 0x5302, "hid-type2 bulk", &fp);
        assert!(message.contains("bcd_device=4.07"));
        assert!(message.contains("wMaxPacketSize=8"));
        assert!(message.contains("observed bulk pair: none"));
        assert!(message.contains("class=3"));
    }

    #[test]
    fn endpoint_packet_size_is_not_treated_as_report_length() {
        let fp = fingerprint(vec![iface(
            0,
            USB_CLASS_HID,
            vec![endpoint(
                0x81,
                UsbDirection::In,
                UsbTransferKind::Interrupt,
                8,
            )],
        )]);
        let hid = hid_interrupt_in_endpoints(&fp)[0];
        assert_eq!(hid.max_packet_size, 8);
        assert_ne!(hid.max_packet_size, 512);
        assert_ne!(hid.max_packet_size, 513);
    }
}
