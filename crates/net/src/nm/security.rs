//! The security type of an access point or a saved profile.

use crate::model::Security;

/// `NM80211ApFlags`
pub(crate) const AP_PRIVACY: u32 = 0x1;

/// `NM80211ApSecurityFlags`
pub(crate) mod ap {
    pub const PAIR_WEP40: u32 = 0x1;
    pub const PAIR_WEP104: u32 = 0x2;
    pub const PAIR_TKIP: u32 = 0x4;
    pub const PAIR_CCMP: u32 = 0x8;
    pub const GROUP_WEP40: u32 = 0x10;
    pub const GROUP_WEP104: u32 = 0x20;
    pub const GROUP_TKIP: u32 = 0x40;
    pub const GROUP_CCMP: u32 = 0x80;
    pub const KEY_MGMT_PSK: u32 = 0x100;
    pub const KEY_MGMT_8021X: u32 = 0x200;
    pub const KEY_MGMT_SAE: u32 = 0x400;
    pub const KEY_MGMT_OWE: u32 = 0x800;
    pub const KEY_MGMT_OWE_TM: u32 = 0x1000;
    pub const KEY_MGMT_EAP_SUITE_B_192: u32 = 0x2000;
}

/// `NMDeviceWifiCapabilities`
pub(crate) mod caps {
    pub const CIPHER_WEP40: u32 = 0x1;
    pub const CIPHER_WEP104: u32 = 0x2;
    pub const CIPHER_TKIP: u32 = 0x4;
    pub const CIPHER_CCMP: u32 = 0x8;
    pub const WPA: u32 = 0x10;
    pub const RSN: u32 = 0x20;
    pub const IBSS_RSN: u32 = 0x2000;
}

// From most to least secure.
const PREFERENCE: [Security; 11] = [
    Security::Wpa3SuiteB192,
    Security::Sae,
    Security::Wpa2Eap,
    Security::Wpa2Psk,
    Security::WpaEap,
    Security::WpaPsk,
    Security::StaticWep,
    Security::DynamicWep,
    Security::Leap,
    Security::Owe,
    Security::Open,
];

pub(crate) fn best_ap_security(caps: u32, adhoc: bool, flags: u32, wpa: u32, rsn: u32) -> Security {
    PREFERENCE
        .into_iter()
        .find(|&security| is_valid(security, caps, adhoc, flags, wpa, rsn))
        .unwrap_or(Security::Unknown)
}

fn is_valid(security: Security, caps: u32, adhoc: bool, flags: u32, wpa: u32, rsn: u32) -> bool {
    let has = |set: u32, bit: u32| set & bit != 0;
    let pair_cipher = |ap_flags: u32| {
        (has(ap_flags, ap::PAIR_TKIP) && has(caps, caps::CIPHER_TKIP))
            || (has(ap_flags, ap::PAIR_CCMP) && has(caps, caps::CIPHER_CCMP))
    };
    match security {
        Security::Open => !has(flags, AP_PRIVACY) && wpa == 0 && rsn == 0,
        // As in libnm, LEAP falls through into the static WEP checks.
        Security::Leap if adhoc => false,
        Security::Leap | Security::StaticWep => {
            if !has(flags, AP_PRIVACY) {
                return false;
            }
            if (wpa != 0 || rsn != 0)
                && !supports_ciphers(caps, wpa, Security::StaticWep)
                && !supports_ciphers(caps, rsn, Security::StaticWep)
            {
                return false;
            }
            true
        }
        Security::DynamicWep => {
            if adhoc || rsn != 0 || !has(flags, AP_PRIVACY) {
                return false;
            }
            if wpa != 0
                && (!has(wpa, ap::KEY_MGMT_8021X)
                    || !supports_ciphers(caps, wpa, Security::DynamicWep))
            {
                return false;
            }
            true
        }
        Security::WpaPsk => {
            !adhoc && has(caps, caps::WPA) && has(wpa, ap::KEY_MGMT_PSK) && pair_cipher(wpa)
        }
        Security::Wpa2Psk => {
            if !has(caps, caps::RSN) {
                return false;
            }
            if adhoc {
                has(caps, caps::IBSS_RSN) && has(rsn, ap::PAIR_CCMP) && has(caps, caps::CIPHER_CCMP)
            } else {
                has(rsn, ap::KEY_MGMT_PSK) && pair_cipher(rsn)
            }
        }
        Security::WpaEap => {
            !adhoc
                && has(caps, caps::WPA)
                && has(wpa, ap::KEY_MGMT_8021X)
                && supports_ciphers(caps, wpa, Security::WpaEap)
        }
        Security::Wpa2Eap => {
            !adhoc
                && has(caps, caps::RSN)
                && has(rsn, ap::KEY_MGMT_8021X)
                && supports_ciphers(caps, rsn, Security::Wpa2Eap)
        }
        Security::Sae => {
            if !has(caps, caps::RSN) {
                return false;
            }
            if adhoc {
                has(caps, caps::IBSS_RSN) && has(rsn, ap::PAIR_CCMP) && has(caps, caps::CIPHER_CCMP)
            } else {
                has(rsn, ap::KEY_MGMT_SAE) && pair_cipher(rsn)
            }
        }
        Security::Owe => {
            !adhoc
                && has(caps, caps::RSN)
                && (has(rsn, ap::KEY_MGMT_OWE) || has(rsn, ap::KEY_MGMT_OWE_TM))
        }
        Security::Wpa3SuiteB192 => {
            !adhoc && has(caps, caps::RSN) && has(rsn, ap::KEY_MGMT_EAP_SUITE_B_192)
        }
        Security::Unknown => false,
    }
}

fn supports_ciphers(caps: u32, ap_flags: u32, security: Security) -> bool {
    let both = |cap: u32, flag: u32| caps & cap != 0 && ap_flags & flag != 0;
    let static_wep = security == Security::StaticWep;
    let pair = static_wep
        || both(caps::CIPHER_WEP40, ap::PAIR_WEP40)
        || both(caps::CIPHER_WEP104, ap::PAIR_WEP104)
        || both(caps::CIPHER_TKIP, ap::PAIR_TKIP)
        || both(caps::CIPHER_CCMP, ap::PAIR_CCMP);
    let group = both(caps::CIPHER_WEP40, ap::GROUP_WEP40)
        || both(caps::CIPHER_WEP104, ap::GROUP_WEP104)
        || (!static_wep
            && (both(caps::CIPHER_TKIP, ap::GROUP_TKIP)
                || both(caps::CIPHER_CCMP, ap::GROUP_CCMP)));
    pair && group
}

pub(crate) struct ProfileSecurity<'a, P> {
    pub key_mgmt: &'a str,
    pub auth_alg: &'a str,
    pub proto: P,
}

pub(crate) fn profile_security<'a, P>(group: Option<ProfileSecurity<'a, P>>) -> Security
where
    P: IntoIterator<Item = &'a str>,
{
    let Some(ProfileSecurity { key_mgmt, auth_alg, proto }) = group else {
        return Security::Open;
    };
    // WPA1 counts only when both protocols are listed.
    let (mut wpa, mut rsn) = (false, false);
    if matches!(key_mgmt, "wpa-psk" | "wpa-eap") {
        for proto in proto {
            wpa |= proto == "wpa";
            rsn |= proto == "rsn";
        }
    }
    let wpa1 = wpa && rsn;
    match key_mgmt {
        "none" => Security::StaticWep,
        "ieee8021x" if auth_alg == "leap" => Security::Leap,
        "ieee8021x" => Security::DynamicWep,
        "wpa-psk" if wpa1 => Security::WpaPsk,
        "wpa-psk" => Security::Wpa2Psk,
        "wpa-eap" if wpa1 => Security::WpaEap,
        "wpa-eap" => Security::Wpa2Eap,
        "sae" => Security::Sae,
        "wpa-eap-suite-b-192" => Security::Wpa3SuiteB192,
        "owe" => Security::Owe,
        _ => Security::Open,
    }
}

#[cfg(test)]
mod tests {
    use super::ap::*;
    use super::caps::*;
    use super::*;

    const CAPS: u32 = CIPHER_WEP40 | CIPHER_WEP104 | CIPHER_TKIP | CIPHER_CCMP | WPA | RSN;
    const CCMP: u32 = PAIR_CCMP | GROUP_CCMP;

    fn best(flags: u32, wpa: u32, rsn: u32) -> Security {
        best_ap_security(CAPS, false, flags, wpa, rsn)
    }

    #[test]
    fn open_access_point() {
        assert_eq!(best(0, 0, 0), Security::Open);
    }

    #[test]
    fn wep_access_points() {
        assert_eq!(best(AP_PRIVACY, 0, 0), Security::StaticWep);
        assert_eq!(
            best(AP_PRIVACY, GROUP_WEP104, 0),
            Security::StaticWep
        );
    }

    #[test]
    fn dynamic_wep_when_the_card_lacks_wpa() {
        let wpa = KEY_MGMT_8021X | PAIR_TKIP | GROUP_TKIP;
        let caps = CIPHER_WEP104 | CIPHER_TKIP | RSN;
        assert_eq!(
            best_ap_security(caps, false, AP_PRIVACY, wpa, 0),
            Security::DynamicWep
        );
        assert_eq!(best(AP_PRIVACY, wpa, 0), Security::WpaEap);
    }

    #[test]
    fn wpa_and_wpa2_personal() {
        let psk = KEY_MGMT_PSK | PAIR_TKIP | GROUP_TKIP;
        assert_eq!(best(AP_PRIVACY, psk, 0), Security::WpaPsk);
        assert_eq!(
            best(AP_PRIVACY, 0, KEY_MGMT_PSK | CCMP),
            Security::Wpa2Psk
        );
        assert_eq!(
            best(AP_PRIVACY, psk, KEY_MGMT_PSK | CCMP),
            Security::Wpa2Psk
        );
    }

    #[test]
    fn wpa3_personal_and_transition_mode() {
        assert_eq!(
            best(AP_PRIVACY, 0, KEY_MGMT_SAE | CCMP),
            Security::Sae
        );
        let transition = KEY_MGMT_PSK | KEY_MGMT_SAE | CCMP;
        assert_eq!(best(AP_PRIVACY, 0, transition), Security::Sae);
    }

    #[test]
    fn enterprise() {
        let eap = KEY_MGMT_8021X | CCMP;
        assert_eq!(best(AP_PRIVACY, 0, eap), Security::Wpa2Eap);
        let wpa_eap = KEY_MGMT_8021X | PAIR_TKIP | GROUP_TKIP;
        assert_eq!(best(AP_PRIVACY, wpa_eap, 0), Security::WpaEap);
        assert_eq!(
            best(AP_PRIVACY, 0, KEY_MGMT_EAP_SUITE_B_192 | CCMP),
            Security::Wpa3SuiteB192
        );
    }

    #[test]
    fn enhanced_open() {
        assert_eq!(best(0, 0, KEY_MGMT_OWE | CCMP), Security::Owe);
        assert_eq!(best(0, 0, KEY_MGMT_OWE_TM | CCMP), Security::Owe);
    }

    #[test]
    fn the_device_must_support_the_cipher() {
        let caps = CIPHER_TKIP | WPA | RSN;
        let security = best_ap_security(caps, false, AP_PRIVACY, 0, KEY_MGMT_PSK | CCMP);
        assert_eq!(security, Security::Unknown);
    }

    #[test]
    fn adhoc_rules() {
        assert!(!is_valid(
            Security::Leap,
            CAPS,
            true,
            AP_PRIVACY,
            0,
            0
        ));
        assert!(is_valid(
            Security::Leap,
            CAPS,
            false,
            AP_PRIVACY,
            0,
            0
        ));
        assert_eq!(
            best_ap_security(CAPS, true, AP_PRIVACY, 0, 0),
            Security::StaticWep
        );
        let rsn = PAIR_CCMP | GROUP_CCMP;
        assert!(!is_valid(
            Security::Wpa2Psk,
            CAPS,
            true,
            AP_PRIVACY,
            0,
            rsn
        ));
        assert!(is_valid(
            Security::Wpa2Psk,
            CAPS | IBSS_RSN,
            true,
            AP_PRIVACY,
            0,
            rsn
        ));
    }

    fn profile(key_mgmt: &str, auth_alg: &str, proto: &[&'static str]) -> Security {
        profile_security(Some(ProfileSecurity {
            key_mgmt,
            auth_alg,
            proto: proto.iter().copied(),
        }))
    }

    #[test]
    fn profile_types() {
        assert_eq!(
            profile_security::<[&str; 0]>(None),
            Security::Open
        );
        assert_eq!(profile("", "", &[]), Security::Open);
        assert_eq!(profile("none", "", &[]), Security::StaticWep);
        assert_eq!(profile("ieee8021x", "leap", &[]), Security::Leap);
        assert_eq!(
            profile("ieee8021x", "", &[]),
            Security::DynamicWep
        );
        assert_eq!(profile("wpa-psk", "", &[]), Security::Wpa2Psk);
        assert_eq!(
            profile("wpa-psk", "", &["rsn"]),
            Security::Wpa2Psk
        );
        assert_eq!(
            profile("wpa-psk", "", &["wpa", "rsn"]),
            Security::WpaPsk
        );
        assert_eq!(profile("wpa-eap", "", &[]), Security::Wpa2Eap);
        assert_eq!(
            profile("wpa-eap", "", &["rsn", "wpa"]),
            Security::WpaEap
        );
        assert_eq!(profile("sae", "", &[]), Security::Sae);
        assert_eq!(
            profile("wpa-eap-suite-b-192", "", &[]),
            Security::Wpa3SuiteB192
        );
        assert_eq!(profile("owe", "", &[]), Security::Owe);
    }
}
