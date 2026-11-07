#![allow(clippy::unwrap_used)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use trogon_claims::{ClaimType, WellKnownClaim};

fn hash(value: &ClaimType) -> u64 {
    let mut state = DefaultHasher::new();
    value.hash(&mut state);
    state.finish()
}

#[test]
fn ws_names_preserve_exact_text_equality_and_hash() {
    for name in [
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/authenticationinstant",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/authenticationmethod",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/cookiepath",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/denyonlyprimarysid",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/denyonlyprimarygroupsid",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/denyonlywindowsdevicegroup",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/dsa",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/expiration",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/expired",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/groupsid",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/ispersistent",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/primarygroupsid",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/primarysid",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/role",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/serialnumber",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/userdata",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/version",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/windowsaccountname",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/windowsdeviceclaim",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/windowsdevicegroup",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/windowsuserclaim",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/windowsfqbnversion",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/windowssubauthority",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/anonymous",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/authentication",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/authorizationdecision",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/country",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/dateofbirth",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/dns",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/denyonlysid",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/emailaddress",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/gender",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/givenname",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/hash",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/homephone",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/locality",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/mobilephone",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/name",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/nameidentifier",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/otherphone",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/postalcode",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/rsa",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/sid",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/spn",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/stateorprovince",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/streetaddress",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/surname",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/system",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/thumbprint",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/upn",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/uri",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/webpage",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/x500distinguishedname",
        "http://schemas.xmlsoap.org/ws/2009/09/identity/claims/actor",
    ] {
        let borrowed = ClaimType::try_new(name).unwrap();
        let owned = ClaimType::try_new(name.to_owned()).unwrap();
        assert_eq!(borrowed.as_str(), name);
        assert_eq!(borrowed, owned);
        assert_eq!(hash(&borrowed), hash(&owned));
        #[cfg(not(feature = "ws-claims"))]
        {
            assert_eq!(borrowed.well_known(), None);
            assert_eq!(WellKnownClaim::from_name(name), None);
        }
        #[cfg(feature = "ws-claims")]
        {
            let known = WellKnownClaim::from_name(name).unwrap();
            assert_eq!(known.as_str(), name);
            assert_eq!(borrowed.well_known(), Some(known));
            assert_eq!(borrowed, ClaimType::from(known));
            assert_eq!(hash(&borrowed), hash(&ClaimType::from(known)));
        }
        let altered = name.to_uppercase();
        let custom = ClaimType::try_new(altered.clone()).unwrap();
        assert_eq!(custom.as_str(), altered);
        assert_eq!(custom.well_known(), None);
        assert_ne!(custom, borrowed);
    }
}

#[test]
fn core_discriminants_and_order_remain_stable() {
    assert_eq!(WellKnownClaim::Iss as u16, 0);
    assert_eq!(WellKnownClaim::Groups as u16, 25);
    assert_eq!(WellKnownClaim::Upn as u16, 80);
    assert!(WellKnownClaim::Groups < WellKnownClaim::Upn);
    assert_eq!(WellKnownClaim::from_name("upn"), Some(WellKnownClaim::Upn));
}

#[cfg(feature = "ws-claims")]
#[test]
fn ws_constants_variants_and_discriminants_remain_stable() {
    use trogon_claims::constants::*;
    for (known, name, discriminant) in [
        (WellKnownClaim::WsAuthenticationInstant, WS_AUTHENTICATION_INSTANT, 26),
        (WellKnownClaim::WsAuthenticationMethod, WS_AUTHENTICATION_METHOD, 27),
        (WellKnownClaim::WsCookiePath, WS_COOKIE_PATH, 28),
        (WellKnownClaim::WsDenyOnlyPrimarySid, WS_DENY_ONLY_PRIMARY_SID, 29),
        (
            WellKnownClaim::WsDenyOnlyPrimaryGroupSid,
            WS_DENY_ONLY_PRIMARY_GROUP_SID,
            30,
        ),
        (
            WellKnownClaim::WsDenyOnlyWindowsDeviceGroup,
            WS_DENY_ONLY_WINDOWS_DEVICE_GROUP,
            31,
        ),
        (WellKnownClaim::WsDsa, WS_DSA, 32),
        (WellKnownClaim::WsExpiration, WS_EXPIRATION, 33),
        (WellKnownClaim::WsExpired, WS_EXPIRED, 34),
        (WellKnownClaim::WsGroupSid, WS_GROUP_SID, 35),
        (WellKnownClaim::WsIsPersistent, WS_IS_PERSISTENT, 36),
        (WellKnownClaim::WsPrimaryGroupSid, WS_PRIMARY_GROUP_SID, 37),
        (WellKnownClaim::WsPrimarySid, WS_PRIMARY_SID, 38),
        (WellKnownClaim::WsRole, WS_ROLE, 39),
        (WellKnownClaim::WsSerialNumber, WS_SERIAL_NUMBER, 40),
        (WellKnownClaim::WsUserData, WS_USER_DATA, 41),
        (WellKnownClaim::WsVersion, WS_VERSION, 42),
        (WellKnownClaim::WsWindowsAccountName, WS_WINDOWS_ACCOUNT_NAME, 43),
        (WellKnownClaim::WsWindowsDeviceClaim, WS_WINDOWS_DEVICE_CLAIM, 44),
        (WellKnownClaim::WsWindowsDeviceGroup, WS_WINDOWS_DEVICE_GROUP, 45),
        (WellKnownClaim::WsWindowsUserClaim, WS_WINDOWS_USER_CLAIM, 46),
        (WellKnownClaim::WsWindowsFqbnVersion, WS_WINDOWS_FQBN_VERSION, 47),
        (WellKnownClaim::WsWindowsSubAuthority, WS_WINDOWS_SUB_AUTHORITY, 48),
        (WellKnownClaim::WsAnonymous, WS_ANONYMOUS, 49),
        (WellKnownClaim::WsAuthentication, WS_AUTHENTICATION, 50),
        (WellKnownClaim::WsAuthorizationDecision, WS_AUTHORIZATION_DECISION, 51),
        (WellKnownClaim::WsCountry, WS_COUNTRY, 52),
        (WellKnownClaim::WsDateOfBirth, WS_DATE_OF_BIRTH, 53),
        (WellKnownClaim::WsDns, WS_DNS, 54),
        (WellKnownClaim::WsDenyOnlySid, WS_DENY_ONLY_SID, 55),
        (WellKnownClaim::WsEmail, WS_EMAIL, 56),
        (WellKnownClaim::WsGender, WS_GENDER, 57),
        (WellKnownClaim::WsGivenName, WS_GIVEN_NAME, 58),
        (WellKnownClaim::WsHash, WS_HASH, 59),
        (WellKnownClaim::WsHomePhone, WS_HOME_PHONE, 60),
        (WellKnownClaim::WsLocality, WS_LOCALITY, 61),
        (WellKnownClaim::WsMobilePhone, WS_MOBILE_PHONE, 62),
        (WellKnownClaim::WsName, WS_NAME, 63),
        (WellKnownClaim::WsNameIdentifier, WS_NAME_IDENTIFIER, 64),
        (WellKnownClaim::WsOtherPhone, WS_OTHER_PHONE, 65),
        (WellKnownClaim::WsPostalCode, WS_POSTAL_CODE, 66),
        (WellKnownClaim::WsRsa, WS_RSA, 67),
        (WellKnownClaim::WsSid, WS_SID, 68),
        (WellKnownClaim::WsSpn, WS_SPN, 69),
        (WellKnownClaim::WsStateOrProvince, WS_STATE_OR_PROVINCE, 70),
        (WellKnownClaim::WsStreetAddress, WS_STREET_ADDRESS, 71),
        (WellKnownClaim::WsSurname, WS_SURNAME, 72),
        (WellKnownClaim::WsSystem, WS_SYSTEM, 73),
        (WellKnownClaim::WsThumbprint, WS_THUMBPRINT, 74),
        (WellKnownClaim::WsUpn, WS_UPN, 75),
        (WellKnownClaim::WsUri, WS_URI, 76),
        (WellKnownClaim::WsWebpage, WS_WEBPAGE, 77),
        (WellKnownClaim::WsX500DistinguishedName, WS_X500_DISTINGUISHED_NAME, 78),
        (WellKnownClaim::WsActor, WS_ACTOR, 79),
    ] {
        assert_eq!(known.as_str(), name);
        assert_eq!(WellKnownClaim::from_name(name), Some(known));
        assert_eq!(known as u16, discriminant);
        assert!(WellKnownClaim::Groups < known);
        assert!(known < WellKnownClaim::Upn);
    }
}
