// SPDX-License-Identifier: GPL-3.0-or-later
//
// Type 2 pre-handshake selection and post-response negotiated profile policy.
//
// Upstream PM58/SUB0 `0416:5302 / bcdDevice 4.07` active-I/O evidence is limited to
// thermalright-trcc-linux issue #228 / PR #230 for one reported unit; it must not
// be generalized to other profiles sharing the same VID:PID or firmware BCD.

use super::profile::{WireProtocol, build_device_info};
use super::usb_fingerprint::{UsbFingerprint, derive_bulk_pair, hid_interrupt_in_endpoints};
use anyhow::{Result, bail, ensure};

pub const WINBOND_HID2_VID: u16 = 0x0416;
pub const WINBOND_HID2_PID: u16 = 0x5302;
pub const BCD_DEVICE_407: &str = "4.07";

pub const TYPE2_MAGIC: [u8; 4] = [0xDA, 0xDB, 0xDC, 0xDD];
pub const TYPE2_SHORT_RESPONSE_LEN: usize = 8;
pub const TYPE2_LEGACY_RESPONSE_MIN: usize = 20;
/// Bounded read for the passive 4.07 probe (endpoint packet size is unrelated).
pub const TYPE2_PROBE_READ_BOUND: usize = 512;
pub const TYPE2_RESPONSE_SIZE: usize = 512;

/// Upstream issue cited for PM58/SUB0 HID-report behavior on one 4.07 unit only.
pub const UPSTREAM_407_PM58_ISSUE: &str =
    "https://github.com/Lexonight1/thermalright-trcc-linux/issues/228";
pub const UPSTREAM_407_PM58_PR: &str =
    "https://github.com/Lexonight1/thermalright-trcc-linux/pull/230";

/// Passive fingerprint may select only a conservative pre-handshake path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type2PreHandshakePolicy {
    /// Existing bulk IN/OUT init + full response handshake.
    LegacyBulkInit,
    /// Exact `0416:5302 / 4.07` HID interrupt IN + correlated hidraw: no init/output,
    /// bounded short-response read only (safe semantics are intrinsic to this variant).
    Hid407ReadOnlyProbe,
    /// Observed shape is recorded; stop before handshake I/O.
    StopUnsupportedShape,
}

/// Post-handshake output route (descriptor interrupt OUT is not required for HidReport).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidOutputRoute {
    LegacyBulk,
    HidReport,
}

/// Negotiated lifecycle and output policy derived from observed PM/SUB and response shape.
///
/// `output` is `None` when no active route is evidenced (`active_writes_allowed == false`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Type2NegotiatedPolicy {
    output: Option<HidOutputRoute>,
    keep_single_session: bool,
    portrait_native: bool,
    active_writes_allowed: bool,
}

impl Type2NegotiatedPolicy {
    /// Active output route with upstream- or legacy-evidenced transport semantics.
    pub fn authorized(
        output: HidOutputRoute,
        keep_single_session: bool,
        portrait_native: bool,
    ) -> Self {
        Self {
            output: Some(output),
            keep_single_session,
            portrait_native,
            active_writes_allowed: true,
        }
    }

    /// Profile observed but no output route is authorized.
    pub fn observed_inactive() -> Self {
        Self {
            output: None,
            keep_single_session: false,
            portrait_native: false,
            active_writes_allowed: false,
        }
    }

    pub fn output(&self) -> Option<HidOutputRoute> {
        self.output
    }

    pub fn keep_single_session(&self) -> bool {
        self.keep_single_session
    }

    pub fn portrait_native(&self) -> bool {
        self.portrait_native
    }

    pub fn active_writes_allowed(&self) -> bool {
        self.active_writes_allowed
    }
}

/// Observed handshake response plus derived policy for downstream transport wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Type2NegotiatedObservation {
    pm: u8,
    sub: u8,
    response: Vec<u8>,
    policy: Type2NegotiatedPolicy,
}

impl Type2NegotiatedObservation {
    pub fn pm(&self) -> u8 {
        self.pm
    }

    pub fn sub(&self) -> u8 {
        self.sub
    }

    pub fn response(&self) -> &[u8] {
        &self.response
    }

    pub fn policy(&self) -> Type2NegotiatedPolicy {
        self.policy
    }
}

fn is_exact_407_fingerprint(fingerprint: &UsbFingerprint) -> bool {
    fingerprint.vid == WINBOND_HID2_VID
        && fingerprint.pid == WINBOND_HID2_PID
        && fingerprint.bcd_device == BCD_DEVICE_407
}

fn has_hid_interrupt_in(fingerprint: &UsbFingerprint) -> bool {
    !hid_interrupt_in_endpoints(fingerprint).is_empty()
}

/// Select pre-handshake policy from shareable fingerprint facts and hidraw correlation.
pub fn select_type2_pre_handshake_policy(
    fingerprint: &UsbFingerprint,
    hidraw_correlated: bool,
) -> Type2PreHandshakePolicy {
    if is_exact_407_fingerprint(fingerprint) {
        if hidraw_correlated && has_hid_interrupt_in(fingerprint) {
            return Type2PreHandshakePolicy::Hid407ReadOnlyProbe;
        }
        return Type2PreHandshakePolicy::StopUnsupportedShape;
    }
    if derive_bulk_pair(fingerprint).is_some() {
        return Type2PreHandshakePolicy::LegacyBulkInit;
    }
    Type2PreHandshakePolicy::StopUnsupportedShape
}

/// Eight-byte Type2 magic response accepted only on the 4.07 read-only probe path.
pub fn validate_short_response_type2(resp: &[u8]) -> bool {
    resp.len() == TYPE2_SHORT_RESPONSE_LEN && resp[0..4] == TYPE2_MAGIC
}

/// Parse PM/SUB from a short or legacy full Type2 response without guessing.
pub fn parse_type2_pm_sub(response: &[u8]) -> Result<(u8, u8)> {
    if validate_short_response_type2(response) {
        return Ok((response[5], response[4]));
    }
    if response.len() >= TYPE2_LEGACY_RESPONSE_MIN
        && response[0..4] == TYPE2_MAGIC
        && response[12] == 0x01
    {
        return Ok((response[5], response[4]));
    }
    bail!(
        "malformed Type2 response: len={} (need {TYPE2_SHORT_RESPONSE_LEN} short or >={TYPE2_LEGACY_RESPONSE_MIN} legacy)",
        response.len()
    );
}

/// PM/SUB pair evidenced for upstream 4.07 HID-report active I/O (#228 / #230).
pub const TYPE2_PM58_HID_REPORT: u8 = 58;
pub const TYPE2_SUB0: u8 = 0;

/// Authorize active HID-report writes from a negotiated Type2 observation.
pub fn authorize_hid_report_writes(obs: &Type2NegotiatedObservation) -> Result<()> {
    ensure!(
        validate_short_response_type2(obs.response()),
        "HID report write authorization requires {TYPE2_SHORT_RESPONSE_LEN}-byte Type2 magic response"
    );
    ensure!(
        obs.pm() == TYPE2_PM58_HID_REPORT && obs.sub() == TYPE2_SUB0,
        "HID report writes authorized only for PM{TYPE2_PM58_HID_REPORT}/SUB{TYPE2_SUB0}, got PM={} SUB={}",
        obs.pm(),
        obs.sub()
    );
    let policy = obs.policy();
    ensure!(
        policy.active_writes_allowed(),
        "active HID report writes not authorized for PM={} SUB={}",
        obs.pm(),
        obs.sub()
    );
    ensure!(
        policy.output() == Some(HidOutputRoute::HidReport),
        "negotiated output route is not HID report for PM={} SUB={}",
        obs.pm(),
        obs.sub()
    );
    ensure!(
        policy.keep_single_session(),
        "HID report write authorization requires single-session policy for PM={} SUB={}",
        obs.pm(),
        obs.sub()
    );
    Ok(())
}

/// Derive negotiated policy from response bytes and the selected pre-handshake path.
pub fn negotiate_type2_policy(
    vid: u16,
    pid: u16,
    response: &[u8],
    pre: Type2PreHandshakePolicy,
) -> Result<Type2NegotiatedObservation> {
    let (pm, sub) = parse_type2_pm_sub(response)?;

    match pre {
        Type2PreHandshakePolicy::Hid407ReadOnlyProbe => {
            ensure!(
                validate_short_response_type2(response),
                "4.07 probe requires {TYPE2_SHORT_RESPONSE_LEN}-byte response, got {}",
                response.len()
            );
            if pm == 58 && sub == 0 {
                // #228 / #230: HID report output, skip-init, portrait-native 240×320, one session.
                return Ok(Type2NegotiatedObservation {
                    pm,
                    sub,
                    response: response.to_vec(),
                    policy: Type2NegotiatedPolicy::authorized(
                        HidOutputRoute::HidReport,
                        true,
                        true,
                    ),
                });
            }
            // Known profile observed on 4.07; active output not evidenced for this PM/SUB.
            build_device_info(WireProtocol::HidType2, vid, pid, pm, sub, None)?;
            Ok(Type2NegotiatedObservation {
                pm,
                sub,
                response: response.to_vec(),
                policy: Type2NegotiatedPolicy::observed_inactive(),
            })
        }
        Type2PreHandshakePolicy::LegacyBulkInit => {
            ensure!(
                response.len() >= TYPE2_LEGACY_RESPONSE_MIN
                    && response[0..4] == TYPE2_MAGIC
                    && response[12] == 0x01,
                "legacy Type2 handshake requires full response (got {} bytes)",
                response.len()
            );
            build_device_info(WireProtocol::HidType2, vid, pid, pm, sub, None)?;
            Ok(Type2NegotiatedObservation {
                pm,
                sub,
                response: response.to_vec(),
                policy: Type2NegotiatedPolicy::authorized(HidOutputRoute::LegacyBulk, false, false),
            })
        }
        Type2PreHandshakePolicy::StopUnsupportedShape => {
            bail!("pre-handshake policy forbids negotiation for unsupported shape");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::usb_fingerprint::{
        UsbDirection, UsbEndpointCapability, UsbInterfaceShape, UsbTransferKind,
    };

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

    fn fingerprint_407_hid_in() -> UsbFingerprint {
        UsbFingerprint {
            vid: WINBOND_HID2_VID,
            pid: WINBOND_HID2_PID,
            bcd_device: BCD_DEVICE_407.to_string(),
            interfaces: vec![iface(
                0,
                3,
                vec![endpoint(
                    0x81,
                    UsbDirection::In,
                    UsbTransferKind::Interrupt,
                    8,
                )],
            )],
        }
    }

    fn fingerprint_407_bulk() -> UsbFingerprint {
        UsbFingerprint {
            vid: WINBOND_HID2_VID,
            pid: WINBOND_HID2_PID,
            bcd_device: BCD_DEVICE_407.to_string(),
            interfaces: vec![iface(
                1,
                255,
                vec![
                    endpoint(0x81, UsbDirection::In, UsbTransferKind::Bulk, 512),
                    endpoint(0x02, UsbDirection::Out, UsbTransferKind::Bulk, 512),
                ],
            )],
        }
    }

    fn legacy_full_response(pm: u8, sub: u8) -> Vec<u8> {
        let mut resp = vec![0u8; 20];
        resp[0..4].copy_from_slice(&TYPE2_MAGIC);
        resp[12] = 0x01;
        resp[5] = pm;
        resp[4] = sub;
        resp
    }

    fn short_pm58_response() -> Vec<u8> {
        // Upstream #228 / #230 reported eight-byte response.
        vec![0xDA, 0xDB, 0xDC, 0xDD, 0x00, 0x3A, 0x00, 0x00]
    }

    #[test]
    fn pre_handshake_407_hid_in_correlated_selects_read_only_probe() {
        let policy = select_type2_pre_handshake_policy(&fingerprint_407_hid_in(), true);
        assert_eq!(policy, Type2PreHandshakePolicy::Hid407ReadOnlyProbe);
    }

    #[test]
    fn pre_handshake_407_without_hidraw_correlation_stops() {
        let policy = select_type2_pre_handshake_policy(&fingerprint_407_hid_in(), false);
        assert_eq!(policy, Type2PreHandshakePolicy::StopUnsupportedShape);
    }

    #[test]
    fn pre_handshake_407_without_interrupt_in_stops() {
        let fp = UsbFingerprint {
            vid: WINBOND_HID2_VID,
            pid: WINBOND_HID2_PID,
            bcd_device: BCD_DEVICE_407.to_string(),
            interfaces: vec![iface(
                1,
                255,
                vec![
                    endpoint(0x81, UsbDirection::In, UsbTransferKind::Bulk, 512),
                    endpoint(0x02, UsbDirection::Out, UsbTransferKind::Bulk, 512),
                ],
            )],
        };
        let policy = select_type2_pre_handshake_policy(&fp, true);
        assert_eq!(policy, Type2PreHandshakePolicy::StopUnsupportedShape);
    }

    #[test]
    fn pre_handshake_non_407_bulk_selects_legacy() {
        let fp = UsbFingerprint {
            vid: WINBOND_HID2_VID,
            pid: WINBOND_HID2_PID,
            bcd_device: "1.00".to_string(),
            interfaces: fingerprint_407_bulk().interfaces,
        };
        let policy = select_type2_pre_handshake_policy(&fp, false);
        assert_eq!(policy, Type2PreHandshakePolicy::LegacyBulkInit);
    }

    #[test]
    fn pre_handshake_unrelated_shape_stops() {
        let fp = UsbFingerprint {
            vid: 0x1234,
            pid: 0x5678,
            bcd_device: "1.00".to_string(),
            interfaces: vec![],
        };
        assert_eq!(
            select_type2_pre_handshake_policy(&fp, false),
            Type2PreHandshakePolicy::StopUnsupportedShape
        );
    }

    #[test]
    fn authorize_hid_report_writes_requires_pm58_hid_report_route() {
        let obs = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &short_pm58_response(),
            Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
        )
        .unwrap();
        authorize_hid_report_writes(&obs).unwrap();

        let mut inactive = short_pm58_response();
        inactive[5] = 68;
        let obs = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &inactive,
            Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
        )
        .unwrap();
        let error = authorize_hid_report_writes(&obs).unwrap_err();
        assert!(
            error.to_string().contains("PM58/SUB0")
                || error
                    .to_string()
                    .contains("active HID report writes not authorized"),
            "{error:#}"
        );
    }

    #[test]
    fn authorize_hid_report_writes_rejects_pm68_even_with_magic_response() {
        let mut resp = short_pm58_response();
        resp[5] = 68;
        let obs = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &resp,
            Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
        )
        .unwrap();
        let error = authorize_hid_report_writes(&obs).unwrap_err();
        assert!(
            error.to_string().contains("PM58/SUB0")
                || error
                    .to_string()
                    .contains("active HID report writes not authorized"),
            "{error:#}"
        );
    }

    #[test]
    fn authorize_hid_report_writes_rejects_pm58_legacy_bulk_route() {
        let obs = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &legacy_full_response(58, 0),
            Type2PreHandshakePolicy::LegacyBulkInit,
        )
        .unwrap();
        let error = authorize_hid_report_writes(&obs).unwrap_err();
        assert!(
            error.to_string().contains("not HID report")
                || error.to_string().contains("magic response"),
            "{error:#}"
        );
    }

    #[test]
    fn authorize_hid_report_writes_rejects_pm58_wrong_sub() {
        let mut resp = short_pm58_response();
        resp[4] = 1;
        let obs = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &resp,
            Type2PreHandshakePolicy::Hid407ReadOnlyProbe,
        )
        .unwrap();
        let error = authorize_hid_report_writes(&obs).unwrap_err();
        assert!(
            error.to_string().contains("SUB0") || error.to_string().contains("SUB"),
            "{error:#}"
        );
    }

    #[test]
    fn pm58_short_response_authorizes_upstream_evidenced_policy() {
        let pre = Type2PreHandshakePolicy::Hid407ReadOnlyProbe;
        let obs = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &short_pm58_response(),
            pre,
        )
        .expect("PM58/SUB0");
        assert_eq!(obs.pm(), 58);
        assert_eq!(obs.sub(), 0);
        assert_eq!(
            obs.policy(),
            Type2NegotiatedPolicy::authorized(HidOutputRoute::HidReport, true, true)
        );
    }

    #[test]
    fn pm68_short_response_observed_but_active_writes_disallowed() {
        let mut resp = short_pm58_response();
        resp[5] = 68;
        let pre = Type2PreHandshakePolicy::Hid407ReadOnlyProbe;
        let obs = negotiate_type2_policy(WINBOND_HID2_VID, WINBOND_HID2_PID, &resp, pre).unwrap();
        assert_eq!(obs.pm(), 68);
        assert_eq!(obs.policy(), Type2NegotiatedPolicy::observed_inactive());
    }

    #[test]
    fn unknown_pm_on_407_probe_fails_without_guessing() {
        let mut resp = short_pm58_response();
        resp[5] = 200;
        let pre = Type2PreHandshakePolicy::Hid407ReadOnlyProbe;
        let error =
            negotiate_type2_policy(WINBOND_HID2_VID, WINBOND_HID2_PID, &resp, pre).unwrap_err();
        assert!(
            error.to_string().contains("unsupported HID Type2 PM=200"),
            "{error:#}"
        );
    }

    #[test]
    fn eight_byte_response_rejected_on_legacy_path() {
        let pre = Type2PreHandshakePolicy::LegacyBulkInit;
        let error = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &short_pm58_response(),
            pre,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("legacy Type2 handshake requires full response"),
            "{error:#}"
        );
    }

    #[test]
    fn legacy_pm49_keeps_bulk_active_policy() {
        let pre = Type2PreHandshakePolicy::LegacyBulkInit;
        let obs = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &legacy_full_response(49, 0),
            pre,
        )
        .unwrap();
        assert_eq!(obs.pm(), 49);
        assert_eq!(
            obs.policy(),
            Type2NegotiatedPolicy::authorized(HidOutputRoute::LegacyBulk, false, false)
        );
    }

    #[test]
    fn malformed_response_fails_parse() {
        let error = parse_type2_pm_sub(&[0x00; 4]).unwrap_err();
        assert!(
            error.to_string().contains("malformed Type2 response"),
            "{error:#}"
        );
    }

    #[test]
    fn observed_inactive_has_no_output_route() {
        let policy = Type2NegotiatedPolicy::observed_inactive();
        assert!(!policy.active_writes_allowed());
        assert_eq!(policy.output(), None);
    }

    #[test]
    fn stop_pre_handshake_forbids_negotiation() {
        let error = negotiate_type2_policy(
            WINBOND_HID2_VID,
            WINBOND_HID2_PID,
            &short_pm58_response(),
            Type2PreHandshakePolicy::StopUnsupportedShape,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("pre-handshake policy forbids negotiation"),
            "{error:#}"
        );
    }
}
