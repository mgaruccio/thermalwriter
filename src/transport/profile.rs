// SPDX-License-Identifier: GPL-3.0-or-later
//
// Device profile tables and pure geometry/rotation helpers.
// Protocol tables and multi-cooler wire behavior are derived from
// thermalright-trcc-linux at tree 390b880abd4cf0ed2d6eae7151493432263eff39
// (project version 9.8.6, four commits after the v9.8.6 tag), path: src/trcc/core/protocol.py

//! Typed hardware/profile model for full-pixel Thermalright LCD families.

pub use crate::display_geometry::{DisplayShape, display_shape};
use anyhow::{Result, bail};
use std::fmt;

/// Wire protocol a full-pixel LCD speaks over USB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WireProtocol {
    Bulk,
    Scsi,
    HidType2,
    HidType3,
    Ly,
}

impl WireProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bulk => "bulk",
            Self::Scsi => "scsi",
            Self::HidType2 => "hid2",
            Self::HidType3 => "hid3",
            Self::Ly => "ly",
        }
    }
}

impl fmt::Display for WireProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Frame payload encoding negotiated for a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameEncoding {
    Jpeg,
    Rgb565Le,
    Rgb565Be,
}

impl FrameEncoding {
    pub fn is_jpeg(self) -> bool {
        matches!(self, Self::Jpeg)
    }

    pub fn is_rgb565(self) -> bool {
        matches!(self, Self::Rgb565Le | Self::Rgb565Be)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Rgb565Le => "rgb565-le",
            Self::Rgb565Be => "rgb565-be",
        }
    }
}

impl fmt::Display for FrameEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything needed to render + encode for a negotiated panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceProfile {
    pub width: u32,
    pub height: u32,
    pub encoding: FrameEncoding,
    /// Pre-rotate panels (non-square portrait / widescreen families).
    pub rotate_panel: bool,
    pub widescreen: bool,
    /// Device-only mount baseline applied after the content wire angle.
    pub encode_baseline: u16,
    /// Widescreen JPEG encode-table base (SUB overrides folded at resolve).
    pub encode_base: u16,
    /// When true, user rotation is subtracted in the widescreen encode table.
    pub encode_invert: bool,
}

impl DeviceProfile {
    pub fn resolution(self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Negotiated identity + profile for an open LCD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputAuthorization {
    Square87adPm4,
    Type2Pm58,
    Type2Pm128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub vid: u16,
    pub pid: u16,
    pub pm: u8,
    pub sub: u8,
    pub fbl: u8,
    pub protocol: WireProtocol,
    pub profile: DeviceProfile,
    /// Set only by the exact production negotiation path; fixture/profile
    /// constructors intentionally leave this unset.
    pub(crate) authorization: Option<OutputAuthorization>,
}

impl DeviceInfo {
    pub fn width(&self) -> u32 {
        self.profile.width
    }

    pub fn height(&self) -> u32 {
        self.profile.height
    }

    pub fn encoding(&self) -> FrameEncoding {
        self.profile.encoding
    }

    pub fn oriented_dimensions(&self, rotation: u16) -> Result<(u32, u32)> {
        oriented_dimensions(self.width(), self.height(), rotation)
    }

    /// Encoded dimensions expected by the wire transport for every user
    /// orientation. The profile's baseline mount rotation is applied during
    /// encode, so non-square rotate-panel families may swap the native axes.
    pub fn wire_dimensions(&self) -> Result<(u32, u32)> {
        let baseline = wire_angle(&self.profile, 0)?;
        oriented_dimensions(self.width(), self.height(), baseline)
    }

    pub(crate) fn authorized(
        vid: u16,
        pid: u16,
        pm: u8,
        sub: u8,
        fbl: u8,
        protocol: WireProtocol,
        width: u32,
        height: u32,
        encoding: FrameEncoding,
        policy: crate::transport::policy::ExactDevicePolicy,
    ) -> Self {
        let authorization = Some(match policy {
            crate::transport::policy::ExactDevicePolicy::Square87adPm4 => {
                OutputAuthorization::Square87adPm4
            }
            crate::transport::policy::ExactDevicePolicy::Type2Pm58 => {
                OutputAuthorization::Type2Pm58
            }
            crate::transport::policy::ExactDevicePolicy::Type2Pm128 => {
                OutputAuthorization::Type2Pm128
            }
        });
        Self {
            vid,
            pid,
            pm,
            sub,
            fbl,
            protocol,
            profile: DeviceProfile {
                width,
                height,
                encoding,
                rotate_panel: false,
                widescreen: false,
                encode_baseline: 0,
                encode_base: 0,
                encode_invert: false,
            },
            authorization,
        }
    }

    pub(crate) fn authorized_policy(&self) -> Option<crate::transport::policy::ExactDevicePolicy> {
        match self.authorization {
            Some(OutputAuthorization::Square87adPm4) => {
                Some(crate::transport::policy::ExactDevicePolicy::Square87adPm4)
            }
            Some(OutputAuthorization::Type2Pm58) => {
                Some(crate::transport::policy::ExactDevicePolicy::Type2Pm58)
            }
            Some(OutputAuthorization::Type2Pm128) => {
                Some(crate::transport::policy::ExactDevicePolicy::Type2Pm128)
            }
            None => None,
        }
    }
    /// Stable fixture id: `<wire>-<vid4>-<pid4>-pm<n>-sub<n>-fbl<n>`.
    pub fn fixture_id(&self) -> String {
        format!(
            "{}-{:04x}-{:04x}-pm{}-sub{}-fbl{}",
            self.protocol.as_str(),
            self.vid,
            self.pid,
            self.pm,
            self.sub,
            self.fbl
        )
    }
}

// ---------------------------------------------------------------------------
// FBL base table (pinned upstream FBL_PROFILES)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct FblBase {
    width: u32,
    height: u32,
    encoding: FrameEncoding,
    rotate_panel: bool,
    widescreen: bool,
    encode_base: u16,
    encode_invert: bool,
    /// ((pm, baseline_deg), ...)
    encode_pm_bases: &'static [(u8, u16)],
    /// ((sub, base_deg), ...)
    encode_sub_bases: &'static [(u8, u16)],
}

const FBL_36: FblBase = FblBase {
    width: 240,
    height: 240,
    encoding: FrameEncoding::Rgb565Le,
    rotate_panel: false,
    widescreen: false,
    encode_base: 0,
    encode_invert: true,
    encode_pm_bases: &[],
    encode_sub_bases: &[],
};
const FBL_37: FblBase = FBL_36;
const FBL_50: FblBase = FblBase {
    width: 320,
    height: 240,
    encoding: FrameEncoding::Rgb565Le,
    rotate_panel: true,
    widescreen: false,
    encode_base: 0,
    encode_invert: true,
    encode_pm_bases: &[],
    encode_sub_bases: &[],
};
const FBL_51: FblBase = FBL_50;
const FBL_52: FblBase = FBL_50;
const FBL_53: FblBase = FBL_50;
const FBL_54: FblBase = FblBase {
    width: 360,
    height: 360,
    encoding: FrameEncoding::Jpeg,
    rotate_panel: false,
    widescreen: false,
    encode_base: 0,
    encode_invert: true,
    encode_pm_bases: &[],
    encode_sub_bases: &[],
};
const FBL_58: FblBase = FBL_50;
const FBL_64: FblBase = FblBase {
    width: 640,
    height: 480,
    encoding: FrameEncoding::Rgb565Le,
    rotate_panel: true,
    widescreen: false,
    encode_base: 0,
    encode_invert: true,
    encode_pm_bases: &[],
    encode_sub_bases: &[],
};
const FBL_72: FblBase = FblBase {
    width: 480,
    height: 480,
    encoding: FrameEncoding::Rgb565Le,
    rotate_panel: false,
    widescreen: false,
    encode_base: 0,
    encode_invert: true,
    encode_pm_bases: &[(6, 180)],
    encode_sub_bases: &[],
};
const FBL_100: FblBase = FblBase {
    width: 320,
    height: 320,
    encoding: FrameEncoding::Rgb565Be,
    rotate_panel: false,
    widescreen: false,
    encode_base: 0,
    encode_invert: true,
    encode_pm_bases: &[],
    encode_sub_bases: &[],
};
const FBL_101: FblBase = FBL_100;
const FBL_102: FblBase = FBL_100;
const FBL_114: FblBase = FblBase {
    width: 1600,
    height: 720,
    encoding: FrameEncoding::Jpeg,
    rotate_panel: true,
    widescreen: true,
    encode_base: 180,
    encode_invert: true,
    encode_pm_bases: &[],
    encode_sub_bases: &[(3, 0)],
};
const FBL_128: FblBase = FblBase {
    width: 1280,
    height: 480,
    encoding: FrameEncoding::Jpeg,
    rotate_panel: true,
    widescreen: true,
    // TRCC parity: base 0, invert true, no phantom sub overrides (#203).
    encode_base: 0,
    encode_invert: true,
    encode_pm_bases: &[],
    encode_sub_bases: &[],
};
const FBL_129: FblBase = FBL_72;
const FBL_192: FblBase = FblBase {
    width: 1920,
    height: 462,
    encoding: FrameEncoding::Jpeg,
    rotate_panel: true,
    widescreen: true,
    encode_base: 180,
    encode_invert: true,
    encode_pm_bases: &[],
    encode_sub_bases: &[(2, 0), (3, 0), (4, 0)],
};
const FBL_224: FblBase = FblBase {
    width: 854,
    height: 480,
    encoding: FrameEncoding::Jpeg,
    rotate_panel: true,
    widescreen: true,
    encode_base: 0,
    encode_invert: false,
    encode_pm_bases: &[],
    encode_sub_bases: &[(2, 180)],
};

/// Known FBL codes in the pinned upstream table.
pub const KNOWN_FBL_CODES: &[u8] = &[
    36, 37, 50, 51, 52, 53, 54, 58, 64, 72, 100, 101, 102, 114, 128, 129, 192, 224,
];

fn fbl_base(fbl: u8) -> Option<FblBase> {
    Some(match fbl {
        36 => FBL_36,
        37 => FBL_37,
        50 => FBL_50,
        51 => FBL_51,
        52 => FBL_52,
        53 => FBL_53,
        54 => FBL_54,
        58 => FBL_58,
        64 => FBL_64,
        72 => FBL_72,
        100 => FBL_100,
        101 => FBL_101,
        102 => FBL_102,
        114 => FBL_114,
        128 => FBL_128,
        129 => FBL_129,
        192 => FBL_192,
        224 => FBL_224,
        _ => return None,
    })
}

// PM → FBL overrides (PM ≠ FBL). Default is PM=FBL.
fn pm_to_fbl_override(pm: u8) -> Option<u8> {
    Some(match pm {
        5 => 50,
        7 => 64,
        9 | 10 | 11 | 12 | 13 | 15 | 16 | 17 => 224,
        14 => 64,
        32 => 100,
        50 => 50,
        63 | 64 => 114,
        65 | 66 | 68 | 69 => 192,
        _ => return None,
    })
}

fn pm_sub_to_fbl(pm: u8, sub: u8) -> Option<u8> {
    match (pm, sub) {
        (1, 48) => Some(114),
        (1, 49) => Some(192),
        _ => None,
    }
}

/// Map PM/SUB to FBL. Compound (PM, SUB) keys win; else PM overrides; else PM=FBL.
pub fn pm_to_fbl(pm: u8, sub: u8) -> u8 {
    if let Some(fbl) = pm_sub_to_fbl(pm, sub) {
        return fbl;
    }
    pm_to_fbl_override(pm).unwrap_or(pm)
}

fn fbl_224_dims(pm: u8) -> (u32, u32) {
    match pm {
        10 | 16 => (960, 540),
        12 => (800, 480),
        13 | 17 => (960, 320),
        15 => (640, 172),
        _ => (854, 480),
    }
}

fn fbl_192_dims(pm: u8) -> (u32, u32) {
    match pm {
        68 => (1280, 480),
        69 => (1920, 440),
        _ => (1920, 462),
    }
}

fn resolve_encode_base_from_pm(base: FblBase, pm: u8) -> u16 {
    for &(key, deg) in base.encode_pm_bases {
        if key == pm {
            return deg;
        }
    }
    0
}

fn resolve_encode_base_from_sub(base: FblBase, sub: u8) -> u16 {
    for &(key, deg) in base.encode_sub_bases {
        if key == sub {
            return deg;
        }
    }
    base.encode_base
}

/// Bulk PMs that leave the default FBL72 base (plus PM1 SUB48/49).
const BULK_KNOWN_PMS: &[u8] = &[5, 7, 9, 10, 11, 12, 32, 64, 65];

fn bulk_fbl(pm: u8, sub: u8) -> u8 {
    if BULK_KNOWN_PMS.contains(&pm) || (pm == 1 && matches!(sub, 48 | 49)) {
        pm_to_fbl(pm, sub)
    } else {
        72
    }
}

fn apply_family_encoding(
    protocol: WireProtocol,
    pm: u8,
    base_encoding: FrameEncoding,
) -> FrameEncoding {
    match protocol {
        WireProtocol::Bulk => {
            // Raw bulk is JPEG except PM32 RGB565-BE.
            if pm == 32 {
                FrameEncoding::Rgb565Be
            } else {
                FrameEncoding::Jpeg
            }
        }
        WireProtocol::Scsi => {
            // SCSI always RGB565; preserve FBL byte order.
            match base_encoding {
                FrameEncoding::Rgb565Be => FrameEncoding::Rgb565Be,
                _ => FrameEncoding::Rgb565Le,
            }
        }
        WireProtocol::HidType2 | WireProtocol::HidType3 | WireProtocol::Ly => base_encoding,
    }
}

fn profile_from_base(
    base: FblBase,
    width: u32,
    height: u32,
    encoding: FrameEncoding,
    pm: u8,
    sub: u8,
) -> Result<DeviceProfile> {
    if width == 0 || height == 0 {
        bail!("invalid profile dimensions {width}x{height}");
    }
    Ok(DeviceProfile {
        width,
        height,
        encoding,
        rotate_panel: base.rotate_panel,
        widescreen: base.widescreen,
        encode_baseline: resolve_encode_base_from_pm(base, pm),
        encode_base: resolve_encode_base_from_sub(base, sub),
        encode_invert: base.encode_invert,
    })
}

/// Resolve a full device profile for a protocol + fingerprint.
///
/// Rejects unknown/unresolvable combinations with an actionable error.
pub fn resolve_profile(
    protocol: WireProtocol,
    vid: u16,
    pid: u16,
    pm: u8,
    sub: u8,
    fbl: u8,
) -> Result<DeviceProfile> {
    let (resolved_fbl, base) = match protocol {
        WireProtocol::Bulk => {
            // The handshake rejects PM0. Unmapped valid PMs retain the
            // established FBL72-compatible fallback from `bulk_fbl`.
            if pm == 0 {
                bail!("unsupported bulk PM={pm} SUB={sub} for {vid:04x}:{pid:04x}");
            }
            let f = bulk_fbl(pm, sub);
            let b = fbl_base(f).ok_or_else(|| anyhow::anyhow!("missing bulk FBL {f}"))?;
            (f, b)
        }
        WireProtocol::Scsi => {
            let b = fbl_base(fbl).ok_or_else(|| {
                anyhow::anyhow!("unsupported SCSI FBL={fbl} for {vid:04x}:{pid:04x}")
            })?;
            (fbl, b)
        }
        WireProtocol::HidType2 => {
            // HID2 accepts any PM/SUB resolving into FBL_PROFILES (incl. PM72).
            // Recognized PM49/59/60 explicitly use the 320×320 BE default.
            let (f, b) = if matches!(pm, 49 | 59 | 60) {
                (100, FBL_100)
            } else {
                let f = pm_to_fbl(pm, sub);
                if let Some(b) = fbl_base(f) {
                    (f, b)
                } else if let Some(b) = fbl_base(pm) {
                    // Some HID2 devices report FBL-as-PM directly.
                    (pm, b)
                } else {
                    bail!("unsupported HID Type2 PM={pm} SUB={sub} for {vid:04x}:{pid:04x}");
                }
            };
            (f, b)
        }
        WireProtocol::HidType3 => {
            if !matches!(fbl, 100 | 101) {
                bail!("unsupported HID Type3 FBL={fbl} for {vid:04x}:{pid:04x}");
            }
            let b = fbl_base(fbl).unwrap();
            (fbl, b)
        }
        WireProtocol::Ly => {
            let f = if fbl != 0 { fbl } else { pm_to_fbl(pm, sub) };
            let b = fbl_base(f).ok_or_else(|| {
                anyhow::anyhow!("unsupported LY FBL={f} PM={pm} SUB={sub} for {vid:04x}:{pid:04x}")
            })?;
            (f, b)
        }
    };

    let (width, height) = match resolved_fbl {
        224 => fbl_224_dims(pm),
        192 => fbl_192_dims(pm),
        _ => (base.width, base.height),
    };

    let encoding = apply_family_encoding(protocol, pm, base.encoding);
    profile_from_base(base, width, height, encoding, pm, sub)
}

/// Build DeviceInfo after handshake, resolving FBL when the protocol did not
/// surface one (bulk/HID2/LY).
pub fn build_device_info(
    protocol: WireProtocol,
    vid: u16,
    pid: u16,
    pm: u8,
    sub: u8,
    fbl: Option<u8>,
) -> Result<DeviceInfo> {
    let fbl = fbl.unwrap_or_else(|| match protocol {
        WireProtocol::Bulk => bulk_fbl(pm, sub),
        WireProtocol::HidType2 if matches!(pm, 49 | 59 | 60) => 100,
        WireProtocol::HidType2 => {
            let f = pm_to_fbl(pm, sub);
            if fbl_base(f).is_some() { f } else { pm }
        }
        WireProtocol::Ly => pm_to_fbl(pm, sub),
        _ => pm_to_fbl(pm, sub),
    });
    let profile = resolve_profile(protocol, vid, pid, pm, sub, fbl)?;
    Ok(DeviceInfo {
        vid,
        pid,
        pm,
        sub,
        fbl,
        protocol,
        profile,
        authorization: None,
    })
}

/// Oriented canvas dimensions for a user rotation of 0/90/180/270.
pub fn oriented_dimensions(width: u32, height: u32, rotation: u16) -> Result<(u32, u32)> {
    match rotation {
        0 | 180 => {
            if width == 0 || height == 0 {
                bail!("invalid dimensions {width}x{height}");
            }
            Ok((width, height))
        }
        90 | 270 => {
            if width == 0 || height == 0 {
                bail!("invalid dimensions {width}x{height}");
            }
            Ok((height, width))
        }
        other => bail!("invalid rotation {other}; only 0/90/180/270 are supported"),
    }
}

/// Wire content angle applied once before encode.
///
/// * non-wide rotate panels: `(base - rotation) % 360` with base 90 only for
///   320×240 RGB565; other non-wide rotate panels use base 0
/// * widescreen JPEG: `(encode_base + signed_rotation) % 360` (negated when
///   `encode_invert`)
/// * others: `(360 - rotation) % 360`
/// * finally add `encode_baseline`
pub fn wire_angle(profile: &DeviceProfile, rotation: u16) -> Result<u16> {
    if !matches!(rotation, 0 | 90 | 180 | 270) {
        bail!("invalid rotation {rotation}; only 0/90/180/270 are supported");
    }
    let rotation = u32::from(rotation);
    let content = if profile.rotate_panel && !profile.widescreen {
        let base: u32 =
            if profile.width == 320 && profile.height == 240 && profile.encoding.is_rgb565() {
                90
            } else {
                0
            };
        (base + 360 - rotation) % 360
    } else if profile.rotate_panel && profile.widescreen && profile.encoding.is_jpeg() {
        let signed = if profile.encode_invert {
            (360 - rotation) % 360
        } else {
            rotation
        };
        (u32::from(profile.encode_base) + signed) % 360
    } else if rotation == 0 {
        0
    } else {
        (360 - rotation) % 360
    };
    let angle = (content + u32::from(profile.encode_baseline)) % 360;
    Ok(angle as u16)
}

// ---------------------------------------------------------------------------
// Fixture-backed profiles for null/capture/tests
// ---------------------------------------------------------------------------

/// Fixture-backed profiles accepted by `known_fixture_profiles()` / null capture.
#[derive(Debug, Clone, Copy)]
pub struct FixtureProfile {
    pub id: &'static str,
    pub protocol: WireProtocol,
    pub vid: u16,
    pub pid: u16,
    pub pm: u8,
    pub sub: u8,
    pub fbl: u8,
}

const FIXTURES: &[FixtureProfile] = &[
    // Bulk Grand Vision 480×480 (default PM4)
    FixtureProfile {
        id: "bulk-87ad-70db-pm4-sub5-fbl72",
        protocol: WireProtocol::Bulk,
        vid: 0x87ad,
        pid: 0x70db,
        pm: 4,
        sub: 5,
        fbl: 72,
    },
    // Bulk PM32 RGB565-BE 320×320
    FixtureProfile {
        id: "bulk-87ad-70db-pm32-sub0-fbl100",
        protocol: WireProtocol::Bulk,
        vid: 0x87ad,
        pid: 0x70db,
        pm: 32,
        sub: 0,
        fbl: 100,
    },
    // Bulk PM64 → 1600×720 JPEG wide
    FixtureProfile {
        id: "bulk-87ad-70db-pm64-sub0-fbl114",
        protocol: WireProtocol::Bulk,
        vid: 0x87ad,
        pid: 0x70db,
        pm: 64,
        sub: 0,
        fbl: 114,
    },
    // Bulk PM5 320×240 JPEG
    FixtureProfile {
        id: "bulk-87ad-70db-pm5-sub0-fbl50",
        protocol: WireProtocol::Bulk,
        vid: 0x87ad,
        pid: 0x70db,
        pm: 5,
        sub: 0,
        fbl: 50,
    },
    // SCSI 320×320 RGB565-BE
    FixtureProfile {
        id: "scsi-87cd-70db-pm100-sub0-fbl100",
        protocol: WireProtocol::Scsi,
        vid: 0x87cd,
        pid: 0x70db,
        pm: 100,
        sub: 0,
        fbl: 100,
    },
    FixtureProfile {
        id: "scsi-0402-3922-pm100-sub0-fbl100",
        protocol: WireProtocol::Scsi,
        vid: 0x0402,
        pid: 0x3922,
        pm: 100,
        sub: 0,
        fbl: 100,
    },
    // HID Type 2
    FixtureProfile {
        id: "hid2-0416-5302-pm58-sub0-fbl58",
        protocol: WireProtocol::HidType2,
        vid: 0x0416,
        pid: 0x5302,
        pm: 58,
        sub: 0,
        fbl: 58,
    },
    FixtureProfile {
        id: "hid2-0416-5302-pm49-sub0-fbl100",
        protocol: WireProtocol::HidType2,
        vid: 0x0416,
        pid: 0x5302,
        pm: 49,
        sub: 0,
        fbl: 100,
    },
    // HID Type 3
    FixtureProfile {
        id: "hid3-0418-5303-pm100-sub0-fbl100",
        protocol: WireProtocol::HidType3,
        vid: 0x0418,
        pid: 0x5303,
        pm: 100,
        sub: 0,
        fbl: 100,
    },
    FixtureProfile {
        id: "hid3-0418-5304-pm101-sub0-fbl101",
        protocol: WireProtocol::HidType3,
        vid: 0x0418,
        pid: 0x5304,
        pm: 101,
        sub: 0,
        fbl: 101,
    },
    // LY Trofeo Vision
    FixtureProfile {
        id: "ly-0416-5408-pm65-sub3-fbl192",
        protocol: WireProtocol::Ly,
        vid: 0x0416,
        pid: 0x5408,
        pm: 65,
        sub: 3,
        fbl: 192,
    },
    // HID2 PM68 1280×480 wide — the upstream fingerprint for the ordered
    // Thermalright Trofeo Vision 6.86-inch panel. The shared VID:PID and
    // PM68/FBL192 path are fixture-covered; SUB, wire angle, and firmware
    // quirks remain capture-pending on this physical unit.
    FixtureProfile {
        id: "hid2-0416-5302-pm68-sub0-fbl192",
        protocol: WireProtocol::HidType2,
        vid: 0x0416,
        pid: 0x5302,
        pm: 68,
        sub: 0,
        fbl: 192,
    },
    FixtureProfile {
        id: "ly-0416-5409-pm50-sub0-fbl50",
        protocol: WireProtocol::Ly,
        vid: 0x0416,
        pid: 0x5409,
        pm: 50,
        sub: 0,
        fbl: 50,
    },
    // Winbond bulk 0416:5406
    FixtureProfile {
        id: "bulk-0416-5406-pm32-sub0-fbl100",
        protocol: WireProtocol::Bulk,
        vid: 0x0416,
        pid: 0x5406,
        pm: 32,
        sub: 0,
        fbl: 100,
    },
];

/// All fixture-backed profile ids (and only those).
pub fn known_fixture_profiles() -> &'static [FixtureProfile] {
    FIXTURES
}

/// Look up a fixture by exact id. Rejects non-fixture ids.
pub fn fixture_by_id(id: &str) -> Result<&'static FixtureProfile> {
    FIXTURES
        .iter()
        .find(|f| f.id == id)
        .ok_or_else(|| anyhow::anyhow!("unknown fixture profile id {id:?}"))
}

/// Resolve a fixture id into a full DeviceInfo.
pub fn device_info_from_fixture(id: &str) -> Result<DeviceInfo> {
    let f = fixture_by_id(id)?;
    build_device_info(f.protocol, f.vid, f.pid, f.pm, f.sub, Some(f.fbl))
}

/// Unique supported native resolutions from the pinned profile matrix.
pub fn supported_resolutions() -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<(u32, u32)>, w: u32, h: u32| {
        if !out.contains(&(w, h)) {
            out.push((w, h));
        }
    };
    for &fbl in KNOWN_FBL_CODES {
        let base = fbl_base(fbl).unwrap();
        match fbl {
            224 => {
                for pm in [9u8, 10, 11, 12, 13, 15, 16, 17] {
                    let (w, h) = fbl_224_dims(pm);
                    push(&mut out, w, h);
                }
            }
            192 => {
                for pm in [65u8, 68, 69] {
                    let (w, h) = fbl_192_dims(pm);
                    push(&mut out, w, h);
                }
            }
            _ => push(&mut out, base.width, base.height),
        }
    }
    // FBL58's 320×240 panel is also mounted in portrait by supported models.
    // Keep both native orientations in the accepted visual matrix.
    push(&mut out, 240, 320);
    out.sort_unstable();
    out
}
