// SPDX-License-Identifier: GPL-3.0-or-later

//! Exact production device-policy registry.
//!
//! Fixture/profile tables elsewhere in the transport module are deliberately
//! broader than this registry.  This is the only registry that can authorize
//! hardware output.

use anyhow::{Result, bail};

use super::profile::{DeviceInfo, FrameEncoding, WireProtocol};
use super::usb_fingerprint::{UsbDirection, UsbFingerprint, UsbTransferKind};

pub const SQUARE_VID: u16 = 0x87ad;
pub const SQUARE_PID: u16 = 0x70db;
pub const WINBOND_VID: u16 = 0x0416;
pub const WINBOND_PID: u16 = 0x5302;
pub const EXACT_BCD_407: &str = "4.07";

pub const PM58_RESPONSE: [u8; 8] = [0xda, 0xdb, 0xdc, 0xdd, 0x00, 0x3a, 0x00, 0x00];
pub const PM128_RESPONSE: [u8; 36] = [
    0xda, 0xdb, 0xdc, 0xdd, 0x01, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x10, 0x00, 0x00, 0x00, 0x41, 0x50, 0x35, 0x33, 0x30, 0x30, 0x30, 0x01, 0x00, 0x67, 0x25, 0x5b,
    0x03, 0x00, 0x77, 0x78,
];

/// The three and only three production output policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactDevicePolicy {
    Square87adPm4,
    Type2Pm58,
    Type2Pm128,
}

/// The bounded operation permitted before response negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbePolicy {
    SquareBulk,
    Type2PassiveHidraw,
}

/// Descriptor-only identity. Type2 is intentionally shared until response negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactDescriptorPolicy {
    Square87ad,
    Type2,
}

impl ExactDevicePolicy {
    pub const fn probe(self) -> ProbePolicy {
        match self {
            Self::Square87adPm4 => ProbePolicy::SquareBulk,
            Self::Type2Pm58 | Self::Type2Pm128 => ProbePolicy::Type2PassiveHidraw,
        }
    }

    pub const fn protocol(self) -> WireProtocol {
        match self {
            Self::Square87adPm4 => WireProtocol::Bulk,
            Self::Type2Pm58 | Self::Type2Pm128 => WireProtocol::HidType2,
        }
    }

    pub const fn dimensions(self) -> (u32, u32) {
        match self {
            Self::Square87adPm4 => (480, 480),
            Self::Type2Pm58 => (240, 320),
            Self::Type2Pm128 => (1280, 480),
        }
    }

    pub const fn encoding(self) -> FrameEncoding {
        match self {
            Self::Square87adPm4 | Self::Type2Pm128 => FrameEncoding::Jpeg,
            Self::Type2Pm58 => FrameEncoding::Rgb565Le,
        }
    }

    pub const fn is_hidraw(self) -> bool {
        matches!(self, Self::Type2Pm58)
    }

    pub const fn is_single_interrupt_frame(self) -> bool {
        matches!(self, Self::Type2Pm128)
    }

    pub(crate) fn device_info(self) -> DeviceInfo {
        let (width, height) = self.dimensions();
        DeviceInfo::authorized(
            self.vid(),
            self.pid(),
            self.pm(),
            self.sub(),
            self.fbl(),
            self.protocol(),
            width,
            height,
            self.encoding(),
            self,
        )
    }

    pub const fn vid(self) -> u16 {
        match self {
            Self::Square87adPm4 => SQUARE_VID,
            Self::Type2Pm58 | Self::Type2Pm128 => WINBOND_VID,
        }
    }

    pub const fn pid(self) -> u16 {
        match self {
            Self::Square87adPm4 => SQUARE_PID,
            Self::Type2Pm58 | Self::Type2Pm128 => WINBOND_PID,
        }
    }

    pub const fn pm(self) -> u8 {
        match self {
            Self::Square87adPm4 => 4,
            Self::Type2Pm58 => 58,
            Self::Type2Pm128 => 128,
        }
    }

    pub const fn sub(self) -> u8 {
        match self {
            Self::Square87adPm4 => 5,
            Self::Type2Pm58 => 0,
            Self::Type2Pm128 => 1,
        }
    }

    pub const fn fbl(self) -> u8 {
        match self {
            Self::Square87adPm4 => 72,
            Self::Type2Pm58 => 58,
            Self::Type2Pm128 => 128,
        }
    }

    pub fn response_matches(self, response: &[u8]) -> bool {
        match self {
            Self::Square87adPm4 => {
                response.len() == 64
                    && response.get(0..4) == Some(&[0x12, 0x34, 0x56, 0x78])
                    && response.get(24) == Some(&4)
                    && response.get(36) == Some(&5)
            }
            Self::Type2Pm58 => response == PM58_RESPONSE,
            Self::Type2Pm128 => response == PM128_RESPONSE,
        }
    }
}

fn endpoint(
    shape: &super::usb_fingerprint::UsbInterfaceShape,
    address: u8,
    direction: UsbDirection,
    transfer: UsbTransferKind,
    mps: u16,
    interval: u8,
) -> bool {
    shape.endpoints.len() == 2
        && shape.endpoints.iter().any(|ep| {
            ep.address == address
                && ep.direction == direction
                && ep.transfer == transfer
                && ep.max_packet_size == mps
                && ep.interval == interval
        })
}

fn exact_square_descriptor(fp: &UsbFingerprint) -> bool {
    if fp.vid != SQUARE_VID
        || fp.pid != SQUARE_PID
        || fp.bcd_device != EXACT_BCD_407
        || fp.interfaces.len() != 1
    {
        return false;
    }
    let iface = &fp.interfaces[0];
    iface.number == 0
        && iface.alternate_setting == 0
        && iface.class == 255
        && iface.subclass == 255
        && iface.protocol == 0
        && endpoint(iface, 0x81, UsbDirection::In, UsbTransferKind::Bulk, 512, 0)
        && endpoint(
            iface,
            0x01,
            UsbDirection::Out,
            UsbTransferKind::Bulk,
            512,
            0,
        )
}

fn exact_type2_descriptor(fp: &UsbFingerprint) -> bool {
    if fp.vid != WINBOND_VID
        || fp.pid != WINBOND_PID
        || fp.bcd_device != EXACT_BCD_407
        || fp.interfaces.len() != 2
    {
        return false;
    }
    let Some(hid) = fp.interfaces.iter().find(|iface| iface.number == 0) else {
        return false;
    };
    let Some(empty) = fp.interfaces.iter().find(|iface| iface.number == 1) else {
        return false;
    };
    hid.alternate_setting == 0
        && hid.class == 3
        && hid.subclass == 0
        && hid.protocol == 0
        && hid.endpoints.len() == 2
        && hid.endpoints.iter().any(|ep| {
            ep.address == 0x83
                && ep.direction == UsbDirection::In
                && ep.transfer == UsbTransferKind::Interrupt
                && ep.max_packet_size == 8
                && ep.interval == 1
        })
        && hid.endpoints.iter().any(|ep| {
            ep.address == 0x02
                && ep.direction == UsbDirection::Out
                && ep.transfer == UsbTransferKind::Interrupt
                && ep.max_packet_size == 512
                && ep.interval == 1
        })
        && empty.alternate_setting == 0
        && empty.class == 255
        && empty.subclass == 255
        && empty.protocol == 255
        && empty.endpoints.is_empty()
}

/// Select only an exact bounded production probe from the complete descriptor.
pub fn select_probe_policy(fp: &UsbFingerprint) -> Result<ProbePolicy> {
    if exact_square_descriptor(fp) {
        return Ok(ProbePolicy::SquareBulk);
    }
    if exact_type2_descriptor(fp) {
        return Ok(ProbePolicy::Type2PassiveHidraw);
    }
    bail!(
        "unsupported exact LCD identity or descriptor shape {:04x}:{:04x} bcd={}",
        fp.vid,
        fp.pid,
        fp.bcd_device
    )
}

/// Return the exact descriptor identity, without negotiating or authorizing output.
pub fn exact_descriptor_policy(fp: &UsbFingerprint) -> Result<ExactDescriptorPolicy> {
    if exact_square_descriptor(fp) {
        Ok(ExactDescriptorPolicy::Square87ad)
    } else if exact_type2_descriptor(fp) {
        Ok(ExactDescriptorPolicy::Type2)
    } else {
        bail!(
            "unsupported exact LCD identity or descriptor shape {:04x}:{:04x} bcd={}",
            fp.vid,
            fp.pid,
            fp.bcd_device
        )
    }
}

/// Resolve a response only after the caller has passed the exact descriptor gate.
pub fn negotiate_response(fp: &UsbFingerprint, response: &[u8]) -> Result<ExactDevicePolicy> {
    match exact_descriptor_policy(fp)? {
        ExactDescriptorPolicy::Square87ad => {
            let policy = ExactDevicePolicy::Square87adPm4;
            if policy.response_matches(response) {
                Ok(policy)
            } else {
                bail!("square response is not the exact 64-byte PM4/SUB5 response")
            }
        }
        ExactDescriptorPolicy::Type2 => {
            if ExactDevicePolicy::Type2Pm58.response_matches(response) {
                Ok(ExactDevicePolicy::Type2Pm58)
            } else if ExactDevicePolicy::Type2Pm128.response_matches(response) {
                Ok(ExactDevicePolicy::Type2Pm128)
            } else {
                bail!("Type2 response is neither exact PM58 nor exact PM128")
            }
        }
    }
}

/// Full policy gate used immediately before a hardware frame write.
pub fn ensure_authorized(info: &DeviceInfo, expected: ExactDevicePolicy) -> Result<()> {
    if info.authorized_policy() != Some(expected) {
        bail!(
            "hardware output requires negotiated exact policy {:?}",
            expected
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::usb_fingerprint::{UsbEndpointCapability, UsbInterfaceShape};

    fn ep(
        address: u8,
        direction: UsbDirection,
        transfer: UsbTransferKind,
        mps: u16,
        interval: u8,
    ) -> UsbEndpointCapability {
        UsbEndpointCapability {
            address,
            direction,
            transfer,
            max_packet_size: mps,
            interval,
        }
    }

    fn type2_fp() -> UsbFingerprint {
        UsbFingerprint {
            vid: WINBOND_VID,
            pid: WINBOND_PID,
            bcd_device: EXACT_BCD_407.into(),
            interfaces: vec![
                UsbInterfaceShape {
                    number: 1,
                    alternate_setting: 0,
                    class: 255,
                    subclass: 255,
                    protocol: 255,
                    endpoints: vec![],
                },
                UsbInterfaceShape {
                    number: 0,
                    alternate_setting: 0,
                    class: 3,
                    subclass: 0,
                    protocol: 0,
                    endpoints: vec![
                        ep(0x02, UsbDirection::Out, UsbTransferKind::Interrupt, 512, 1),
                        ep(0x83, UsbDirection::In, UsbTransferKind::Interrupt, 8, 1),
                    ],
                },
            ],
        }
    }

    fn square_fp() -> UsbFingerprint {
        UsbFingerprint {
            vid: SQUARE_VID,
            pid: SQUARE_PID,
            bcd_device: EXACT_BCD_407.into(),
            interfaces: vec![UsbInterfaceShape {
                number: 0,
                alternate_setting: 0,
                class: 255,
                subclass: 255,
                protocol: 0,
                endpoints: vec![
                    ep(0x81, UsbDirection::In, UsbTransferKind::Bulk, 512, 0),
                    ep(0x01, UsbDirection::Out, UsbTransferKind::Bulk, 512, 0),
                ],
            }],
        }
    }

    #[test]
    fn type2_descriptor_is_order_independent_and_shared() {
        let fp = type2_fp();
        assert_eq!(
            select_probe_policy(&fp).unwrap(),
            ProbePolicy::Type2PassiveHidraw
        );
        assert_eq!(
            exact_descriptor_policy(&fp).unwrap(),
            ExactDescriptorPolicy::Type2
        );
        assert_eq!(
            negotiate_response(&fp, &PM58_RESPONSE).unwrap(),
            ExactDevicePolicy::Type2Pm58
        );
        assert_eq!(
            negotiate_response(&fp, &PM128_RESPONSE).unwrap(),
            ExactDevicePolicy::Type2Pm128
        );
    }

    #[test]
    fn square_descriptor_near_misses_fail_closed() {
        let base = square_fp();
        let mut candidates = Vec::new();
        let mut wrong_bcd = base.clone();
        wrong_bcd.bcd_device = "4.06".into();
        candidates.push(wrong_bcd);
        let mut wrong_iface = base.clone();
        wrong_iface.interfaces[0].alternate_setting = 1;
        candidates.push(wrong_iface);
        let mut wrong_class = base.clone();
        wrong_class.interfaces[0].class = 3;
        candidates.push(wrong_class);
        let mut wrong_in = base.clone();
        wrong_in.interfaces[0].endpoints[0].address = 0x82;
        candidates.push(wrong_in);
        let mut wrong_transfer = base.clone();
        wrong_transfer.interfaces[0].endpoints[0].transfer = UsbTransferKind::Interrupt;
        candidates.push(wrong_transfer);
        let mut wrong_mps = base.clone();
        wrong_mps.interfaces[0].endpoints[0].max_packet_size = 64;
        candidates.push(wrong_mps);
        let mut extra = base;
        extra.interfaces[0].endpoints.push(ep(
            0x82,
            UsbDirection::In,
            UsbTransferKind::Bulk,
            512,
            0,
        ));
        candidates.push(extra);
        for candidate in candidates {
            assert!(select_probe_policy(&candidate).is_err());
        }
    }

    #[test]
    fn type2_descriptor_near_misses_fail_closed() {
        let mut fp = type2_fp();
        for mutate in [0usize, 1, 2, 3, 4, 5, 6] {
            let mut candidate = fp.clone();
            match mutate {
                0 => candidate.bcd_device = "4.06".into(),
                1 => candidate.interfaces[1].alternate_setting = 1,
                2 => candidate.interfaces[1].class = 255,
                3 => candidate.interfaces[1].endpoints[0].address = 0x01,
                4 => candidate.interfaces[1].endpoints[0].transfer = UsbTransferKind::Bulk,
                5 => candidate.interfaces[1].endpoints[0].max_packet_size = 8,
                _ => candidate.interfaces[0].endpoints.push(ep(
                    0x84,
                    UsbDirection::In,
                    UsbTransferKind::Interrupt,
                    8,
                    1,
                )),
            }
            assert!(
                select_probe_policy(&candidate).is_err(),
                "near miss {mutate} accepted"
            );
        }
        fp.interfaces[0].protocol = 254;
        assert!(select_probe_policy(&fp).is_err());
    }

    #[test]
    fn exact_profiles_materialize_wire_geometry() {
        for (policy, dims, encoding) in [
            (
                ExactDevicePolicy::Square87adPm4,
                (480, 480),
                FrameEncoding::Jpeg,
            ),
            (
                ExactDevicePolicy::Type2Pm58,
                (240, 320),
                FrameEncoding::Rgb565Le,
            ),
            (
                ExactDevicePolicy::Type2Pm128,
                (1280, 480),
                FrameEncoding::Jpeg,
            ),
        ] {
            let info = policy.device_info();
            assert_eq!((info.width(), info.height()), dims);
            assert_eq!(info.encoding(), encoding);
            assert_eq!(info.wire_dimensions().unwrap(), dims);
            assert_eq!(info.authorized_policy(), Some(policy));
        }
    }
}
