#![cfg(feature = "daemon")]

//! Device profile model tests: FBL tables, family overrides, shapes, wire angles, fixtures.

use thermalwriter::transport::{
    DisplayShape, FrameEncoding, KNOWN_FBL_CODES, WireProtocol, build_device_info,
    device_info_from_fixture, display_shape, fixture_by_id, known_fixture_profiles,
    oriented_dimensions, pm_to_fbl, resolve_profile, supported_resolutions, wire_angle,
};

#[test]
fn known_fbl_codes_resolve() {
    for &fbl in KNOWN_FBL_CODES {
        let p = resolve_profile(WireProtocol::Scsi, 0x87cd, 0x70db, fbl, 0, fbl)
            .unwrap_or_else(|e| panic!("FBL {fbl}: {e:#}"));
        assert!(p.width > 0 && p.height > 0, "FBL {fbl}");
        assert!(
            matches!(
                p.encoding,
                FrameEncoding::Rgb565Le | FrameEncoding::Rgb565Be
            ),
            "SCSI FBL {fbl} must be RGB565, got {}",
            p.encoding
        );
    }
}

#[test]
fn pm_sub_overrides() {
    assert_eq!(pm_to_fbl(1, 48), 114);
    assert_eq!(pm_to_fbl(1, 49), 192);
    assert_eq!(pm_to_fbl(5, 0), 50);
    assert_eq!(pm_to_fbl(32, 0), 100);
    assert_eq!(pm_to_fbl(64, 0), 114);
    assert_eq!(pm_to_fbl(65, 0), 192);
    assert_eq!(pm_to_fbl(72, 0), 72);
}

#[test]
fn fbl_224_and_192_pm_dims() {
    // Bulk known PMs that map to FBL 224.
    let p10 = resolve_profile(WireProtocol::Bulk, 0x87ad, 0x70db, 10, 0, 224).unwrap();
    assert_eq!((p10.width, p10.height), (960, 540));
    let p12 = resolve_profile(WireProtocol::Bulk, 0x87ad, 0x70db, 12, 0, 224).unwrap();
    assert_eq!((p12.width, p12.height), (800, 480));
    let p9 = resolve_profile(WireProtocol::Bulk, 0x87ad, 0x70db, 9, 0, 224).unwrap();
    assert_eq!((p9.width, p9.height), (854, 480));

    // FBL224 PM-specific dims apply whenever resolved FBL is 224 (SCSI path).
    let p13 = resolve_profile(WireProtocol::Scsi, 0x87cd, 0x70db, 13, 0, 224).unwrap();
    assert_eq!((p13.width, p13.height), (960, 320));
    let p15 = resolve_profile(WireProtocol::Scsi, 0x87cd, 0x70db, 15, 0, 224).unwrap();
    assert_eq!((p15.width, p15.height), (640, 172));
    let p16 = resolve_profile(WireProtocol::Scsi, 0x87cd, 0x70db, 16, 0, 224).unwrap();
    assert_eq!((p16.width, p16.height), (960, 540));

    let p68 = resolve_profile(WireProtocol::Ly, 0x0416, 0x5408, 68, 0, 192).unwrap();
    assert_eq!((p68.width, p68.height), (1280, 480));
    let p69 = resolve_profile(WireProtocol::Ly, 0x0416, 0x5408, 69, 0, 192).unwrap();
    assert_eq!((p69.width, p69.height), (1920, 440));
    let p65 = resolve_profile(WireProtocol::Ly, 0x0416, 0x5408, 65, 3, 192).unwrap();
    assert_eq!((p65.width, p65.height), (1920, 462));
}

#[test]
fn family_encoding_overrides() {
    let bulk = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, None).unwrap();
    assert_eq!(bulk.encoding(), FrameEncoding::Jpeg);
    assert_eq!((bulk.width(), bulk.height()), (480, 480));
    assert_eq!(bulk.fbl, 72);

    let bulk32 = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 32, 0, None).unwrap();
    assert_eq!(bulk32.encoding(), FrameEncoding::Rgb565Be);
    assert_eq!((bulk32.width(), bulk32.height()), (320, 320));

    let scsi = build_device_info(WireProtocol::Scsi, 0x87cd, 0x70db, 100, 0, Some(100)).unwrap();
    assert_eq!(scsi.encoding(), FrameEncoding::Rgb565Be);

    let scsi50 = build_device_info(WireProtocol::Scsi, 0x87cd, 0x70db, 50, 0, Some(50)).unwrap();
    assert_eq!(scsi50.encoding(), FrameEncoding::Rgb565Le);

    for pm in [49u8, 59, 60] {
        let h = build_device_info(WireProtocol::HidType2, 0x0416, 0x5302, pm, 0, None).unwrap();
        assert_eq!((h.width(), h.height()), (320, 320));
        assert_eq!(h.encoding(), FrameEncoding::Rgb565Be);
    }

    let h58 = build_device_info(WireProtocol::HidType2, 0x0416, 0x5302, 58, 0, None).unwrap();
    assert_eq!((h58.width(), h58.height()), (320, 240));
    assert_eq!(h58.encoding(), FrameEncoding::Rgb565Le);

    let h3 = build_device_info(WireProtocol::HidType3, 0x0418, 0x5303, 100, 0, Some(100)).unwrap();
    assert_eq!(h3.encoding(), FrameEncoding::Rgb565Be);
    assert!(build_device_info(WireProtocol::HidType3, 0x0418, 0x5303, 50, 0, Some(50)).is_err());
}

#[test]
fn bulk_unknown_nonzero_pm_falls_back_to_fbl72() {
    let fallback = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 200, 0, None).unwrap();
    assert_eq!(fallback.fbl, 72);
    assert_eq!((fallback.width(), fallback.height()), (480, 480));
    assert_eq!(fallback.encoding(), FrameEncoding::Jpeg);

    assert!(build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 0, 0, None).is_err());
}

#[test]
fn display_shape_boundaries() {
    assert_eq!(display_shape(240, 320).unwrap(), DisplayShape::Portrait);
    assert_eq!(display_shape(320, 320).unwrap(), DisplayShape::Square);
    assert_eq!(display_shape(189, 100).unwrap(), DisplayShape::Landscape);
    assert_eq!(display_shape(190, 100).unwrap(), DisplayShape::Wide);
    assert_eq!(display_shape(274, 100).unwrap(), DisplayShape::Wide);
    assert_eq!(display_shape(275, 100).unwrap(), DisplayShape::Ultrawide);
    assert_eq!(display_shape(1920, 462).unwrap(), DisplayShape::Ultrawide);
    assert!(display_shape(0, 100).is_err());
}

#[test]
fn oriented_dimensions_and_invalid_rotation() {
    assert_eq!(oriented_dimensions(480, 480, 0).unwrap(), (480, 480));
    assert_eq!(oriented_dimensions(480, 480, 180).unwrap(), (480, 480));
    assert_eq!(oriented_dimensions(320, 240, 90).unwrap(), (240, 320));
    assert_eq!(oriented_dimensions(320, 240, 270).unwrap(), (240, 320));
    assert!(oriented_dimensions(320, 240, 45).is_err());
    assert!(oriented_dimensions(0, 240, 0).is_err());
}

#[test]
fn wire_angle_truth_table() {
    let square = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, None).unwrap();
    assert_eq!(wire_angle(&square.profile, 0).unwrap(), 0);
    assert_eq!(wire_angle(&square.profile, 90).unwrap(), 270);

    let pm6 = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 6, 1, None).unwrap();
    assert_eq!(pm6.profile.encode_baseline, 180);
    assert_eq!((pm6.width(), pm6.height()), (480, 480));
    assert_eq!(wire_angle(&pm6.profile, 0).unwrap(), 180);

    let rgb_small = build_device_info(WireProtocol::Scsi, 0x87cd, 0x70db, 50, 0, Some(50)).unwrap();
    assert_eq!((rgb_small.width(), rgb_small.height()), (320, 240));
    assert!(rgb_small.encoding().is_rgb565());
    assert_eq!(wire_angle(&rgb_small.profile, 0).unwrap(), 90);
    assert_eq!(wire_angle(&rgb_small.profile, 90).unwrap(), 0);

    let jpeg_small = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 5, 0, None).unwrap();
    assert_eq!((jpeg_small.width(), jpeg_small.height()), (320, 240));
    assert!(jpeg_small.encoding().is_jpeg());
    assert_eq!(wire_angle(&jpeg_small.profile, 0).unwrap(), 0);
    assert_eq!(wire_angle(&jpeg_small.profile, 90).unwrap(), 270);

    let wide = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 64, 0, None).unwrap();
    assert_eq!((wide.width(), wide.height()), (1600, 720));
    assert_eq!(wide.profile.encode_base, 180);
    assert!(wide.profile.encode_invert);
    assert_eq!(wire_angle(&wide.profile, 0).unwrap(), 180);
    assert_eq!(wire_angle(&wide.profile, 90).unwrap(), 90);

    let f224 = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 9, 0, None).unwrap();
    assert_eq!((f224.width(), f224.height()), (854, 480));
    assert_eq!(f224.profile.encode_base, 0);
    assert!(!f224.profile.encode_invert);
    assert_eq!(wire_angle(&f224.profile, 0).unwrap(), 0);
    assert_eq!(wire_angle(&f224.profile, 90).unwrap(), 90);
}

#[test]
fn fixture_ids_and_lookup() {
    for f in known_fixture_profiles() {
        let info = device_info_from_fixture(f.id).unwrap();
        assert_eq!(info.fixture_id(), f.id);
        assert_eq!(fixture_by_id(f.id).unwrap().id, f.id);
    }
    assert!(fixture_by_id("not-a-real-fixture").is_err());
    assert!(device_info_from_fixture("bulk-0000-0000-pm0-sub0-fbl0").is_err());
}

#[test]
fn ordered_686_inch_1280x480_profile_has_a_fixture() {
    let fixture = fixture_by_id("hid2-0416-5302-pm68-sub0-fbl192").unwrap();
    let info = device_info_from_fixture(fixture.id).unwrap();
    assert_eq!((info.width(), info.height()), (1280, 480));
    assert_eq!(info.protocol, WireProtocol::HidType2);
    assert_eq!((info.vid, info.pid), (0x0416, 0x5302));
    assert_eq!(info.pm, 68);
    assert_eq!(info.fbl, 192);
    assert_eq!(info.encoding(), FrameEncoding::Jpeg);
}

#[test]
fn supported_resolutions_include_matrix() {
    let res = supported_resolutions();
    for want in [
        (240, 240),
        (320, 240),
        (320, 320),
        (360, 360),
        (480, 480),
        (640, 172),
        (640, 480),
        (800, 480),
        (854, 480),
        (960, 320),
        (960, 540),
        (1280, 480),
        (1600, 720),
        (1920, 440),
        (1920, 462),
    ] {
        assert!(res.contains(&want), "missing {want:?} in {res:?}");
    }
}

#[test]
fn invalid_scsi_and_ly_profiles_error() {
    assert!(resolve_profile(WireProtocol::Scsi, 0x87cd, 0x70db, 0, 0, 0).is_err());
    assert!(resolve_profile(WireProtocol::Ly, 0x0416, 0x5408, 1, 0, 1).is_err());
}
