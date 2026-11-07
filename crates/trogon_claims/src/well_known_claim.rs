use crate::constants::*;

/// Exact JWT and OpenID Connect claim names, plus the optional .NET WS vocabulary.
///
/// The `ws-claims` feature enables the `Ws*` variants. Unknown nonempty names
/// remain custom [`crate::ClaimType`] values with their original spelling.
///
/// The [IANA JWT Claims registry](https://www.iana.org/assignments/jwt/jwt.xhtml#claims)
/// identifies each registered name's defining specification. Recognizing a name
/// does not enforce that specification's token validation rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
#[non_exhaustive]
pub enum WellKnownClaim {
    /// Issuer, [RFC 7519 §4.1.1](https://www.rfc-editor.org/rfc/rfc7519.html#section-4.1.1).
    Iss,
    /// Subject, [RFC 7519 §4.1.2](https://www.rfc-editor.org/rfc/rfc7519.html#section-4.1.2).
    Sub,
    /// Audience, [RFC 7519 §4.1.3](https://www.rfc-editor.org/rfc/rfc7519.html#section-4.1.3).
    Aud,
    /// Expiration time, [RFC 7519 §4.1.4](https://www.rfc-editor.org/rfc/rfc7519.html#section-4.1.4).
    Exp,
    /// Not-before time, [RFC 7519 §4.1.5](https://www.rfc-editor.org/rfc/rfc7519.html#section-4.1.5).
    Nbf,
    /// Issued-at time, [RFC 7519 §4.1.6](https://www.rfc-editor.org/rfc/rfc7519.html#section-4.1.6).
    Iat,
    /// JWT identifier, [RFC 7519 §4.1.7](https://www.rfc-editor.org/rfc/rfc7519.html#section-4.1.7).
    Jti,
    /// Current actor and delegation history, [RFC 8693 §4.1](https://www.rfc-editor.org/rfc/rfc8693.html#section-4.1).
    Act,
    /// Actor eligible for delegation, [RFC 8693 §4.4](https://www.rfc-editor.org/rfc/rfc8693.html#section-4.4).
    MayAct,
    /// Proof-of-possession confirmation, [RFC 7800 §3.1](https://www.rfc-editor.org/rfc/rfc7800.html#section-3.1).
    Cnf,
    /// Access-token scope, [RFC 9068 §2.2.3](https://www.rfc-editor.org/rfc/rfc9068.html#section-2.2.3).
    Scope,
    /// OAuth client identifier, [RFC 9068 §2.2](https://www.rfc-editor.org/rfc/rfc9068.html#section-2.2).
    ClientId,
    /// Authentication time, [OpenID Connect Core §2](https://openid.net/specs/openid-connect-core-1_0.html#IDToken).
    AuthTime,
    /// Authentication context class, [OpenID Connect Core §2](https://openid.net/specs/openid-connect-core-1_0.html#IDToken).
    Acr,
    /// Authentication methods, [OpenID Connect Core §2](https://openid.net/specs/openid-connect-core-1_0.html#IDToken)
    /// and [RFC 8176](https://www.rfc-editor.org/rfc/rfc8176.html).
    Amr,
    /// Authorized party, [OpenID Connect Core §2](https://openid.net/specs/openid-connect-core-1_0.html#IDToken).
    Azp,
    /// Session-binding nonce, [OpenID Connect Core §2](https://openid.net/specs/openid-connect-core-1_0.html#IDToken).
    Nonce,
    /// Display name, [OpenID Connect Core §5.1](https://openid.net/specs/openid-connect-core-1_0.html#StandardClaims).
    Name,
    /// Email address, [OpenID Connect Core §5.1](https://openid.net/specs/openid-connect-core-1_0.html#StandardClaims).
    Email,
    /// Email verification assertion, [OpenID Connect Core §5.1](https://openid.net/specs/openid-connect-core-1_0.html#StandardClaims).
    EmailVerified,
    /// Preferred username, [OpenID Connect Core §5.1](https://openid.net/specs/openid-connect-core-1_0.html#StandardClaims).
    PreferredUsername,
    /// Given name, [OpenID Connect Core §5.1](https://openid.net/specs/openid-connect-core-1_0.html#StandardClaims).
    GivenName,
    /// Family name, [OpenID Connect Core §5.1](https://openid.net/specs/openid-connect-core-1_0.html#StandardClaims).
    FamilyName,
    /// Application-defined singular role name; distinct from the registered `roles` claim.
    Role,
    /// Access-token roles, [RFC 9068 §2.2.3.1](https://www.rfc-editor.org/rfc/rfc9068.html#section-2.2.3.1).
    Roles,
    /// Access-token groups, [RFC 9068 §2.2.3.1](https://www.rfc-editor.org/rfc/rfc9068.html#section-2.2.3.1).
    Groups,
    #[cfg(feature = "ws-claims")]
    WsAuthenticationInstant,
    #[cfg(feature = "ws-claims")]
    WsAuthenticationMethod,
    #[cfg(feature = "ws-claims")]
    WsCookiePath,
    #[cfg(feature = "ws-claims")]
    WsDenyOnlyPrimarySid,
    #[cfg(feature = "ws-claims")]
    WsDenyOnlyPrimaryGroupSid,
    #[cfg(feature = "ws-claims")]
    WsDenyOnlyWindowsDeviceGroup,
    #[cfg(feature = "ws-claims")]
    WsDsa,
    #[cfg(feature = "ws-claims")]
    WsExpiration,
    #[cfg(feature = "ws-claims")]
    WsExpired,
    #[cfg(feature = "ws-claims")]
    WsGroupSid,
    #[cfg(feature = "ws-claims")]
    WsIsPersistent,
    #[cfg(feature = "ws-claims")]
    WsPrimaryGroupSid,
    #[cfg(feature = "ws-claims")]
    WsPrimarySid,
    #[cfg(feature = "ws-claims")]
    WsRole,
    #[cfg(feature = "ws-claims")]
    WsSerialNumber,
    #[cfg(feature = "ws-claims")]
    WsUserData,
    #[cfg(feature = "ws-claims")]
    WsVersion,
    #[cfg(feature = "ws-claims")]
    WsWindowsAccountName,
    #[cfg(feature = "ws-claims")]
    WsWindowsDeviceClaim,
    #[cfg(feature = "ws-claims")]
    WsWindowsDeviceGroup,
    #[cfg(feature = "ws-claims")]
    WsWindowsUserClaim,
    #[cfg(feature = "ws-claims")]
    WsWindowsFqbnVersion,
    #[cfg(feature = "ws-claims")]
    WsWindowsSubAuthority,
    #[cfg(feature = "ws-claims")]
    WsAnonymous,
    #[cfg(feature = "ws-claims")]
    WsAuthentication,
    #[cfg(feature = "ws-claims")]
    WsAuthorizationDecision,
    #[cfg(feature = "ws-claims")]
    WsCountry,
    #[cfg(feature = "ws-claims")]
    WsDateOfBirth,
    #[cfg(feature = "ws-claims")]
    WsDns,
    #[cfg(feature = "ws-claims")]
    WsDenyOnlySid,
    #[cfg(feature = "ws-claims")]
    WsEmail,
    #[cfg(feature = "ws-claims")]
    WsGender,
    #[cfg(feature = "ws-claims")]
    WsGivenName,
    #[cfg(feature = "ws-claims")]
    WsHash,
    #[cfg(feature = "ws-claims")]
    WsHomePhone,
    #[cfg(feature = "ws-claims")]
    WsLocality,
    #[cfg(feature = "ws-claims")]
    WsMobilePhone,
    #[cfg(feature = "ws-claims")]
    WsName,
    #[cfg(feature = "ws-claims")]
    WsNameIdentifier,
    #[cfg(feature = "ws-claims")]
    WsOtherPhone,
    #[cfg(feature = "ws-claims")]
    WsPostalCode,
    #[cfg(feature = "ws-claims")]
    WsRsa,
    #[cfg(feature = "ws-claims")]
    WsSid,
    #[cfg(feature = "ws-claims")]
    WsSpn,
    #[cfg(feature = "ws-claims")]
    WsStateOrProvince,
    #[cfg(feature = "ws-claims")]
    WsStreetAddress,
    #[cfg(feature = "ws-claims")]
    WsSurname,
    #[cfg(feature = "ws-claims")]
    WsSystem,
    #[cfg(feature = "ws-claims")]
    WsThumbprint,
    #[cfg(feature = "ws-claims")]
    WsUpn,
    #[cfg(feature = "ws-claims")]
    WsUri,
    #[cfg(feature = "ws-claims")]
    WsWebpage,
    #[cfg(feature = "ws-claims")]
    WsX500DistinguishedName,
    #[cfg(feature = "ws-claims")]
    WsActor,
    /// Principal name from [MicroProfile JWT 2.1](https://download.eclipse.org/microprofile/microprofile-jwt-auth-2.1/microprofile-jwt-auth-spec-2.1.html#_required_mp_jwt_claims).
    // Keep its existing discriminant when optional WS variants are absent.
    Upn = 80,
}

impl WellKnownClaim {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Iss => CLAIM_ISS,
            Self::Sub => CLAIM_SUB,
            Self::Aud => CLAIM_AUD,
            Self::Exp => CLAIM_EXP,
            Self::Nbf => CLAIM_NBF,
            Self::Iat => CLAIM_IAT,
            Self::Jti => CLAIM_JTI,
            Self::Act => CLAIM_ACT,
            Self::MayAct => CLAIM_MAY_ACT,
            Self::Cnf => CLAIM_CNF,
            Self::Scope => CLAIM_SCOPE,
            Self::ClientId => CLAIM_CLIENT_ID,
            Self::AuthTime => CLAIM_AUTH_TIME,
            Self::Acr => CLAIM_ACR,
            Self::Amr => CLAIM_AMR,
            Self::Azp => CLAIM_AZP,
            Self::Nonce => CLAIM_NONCE,
            Self::Name => CLAIM_NAME,
            Self::Email => CLAIM_EMAIL,
            Self::EmailVerified => CLAIM_EMAIL_VERIFIED,
            Self::PreferredUsername => CLAIM_PREFERRED_USERNAME,
            Self::GivenName => CLAIM_GIVEN_NAME,
            Self::FamilyName => CLAIM_FAMILY_NAME,
            Self::Role => CLAIM_ROLE,
            Self::Roles => CLAIM_ROLES,
            Self::Groups => CLAIM_GROUPS,
            Self::Upn => CLAIM_UPN,
            #[cfg(feature = "ws-claims")]
            Self::WsAuthenticationInstant => WS_AUTHENTICATION_INSTANT,
            #[cfg(feature = "ws-claims")]
            Self::WsAuthenticationMethod => WS_AUTHENTICATION_METHOD,
            #[cfg(feature = "ws-claims")]
            Self::WsCookiePath => WS_COOKIE_PATH,
            #[cfg(feature = "ws-claims")]
            Self::WsDenyOnlyPrimarySid => WS_DENY_ONLY_PRIMARY_SID,
            #[cfg(feature = "ws-claims")]
            Self::WsDenyOnlyPrimaryGroupSid => WS_DENY_ONLY_PRIMARY_GROUP_SID,
            #[cfg(feature = "ws-claims")]
            Self::WsDenyOnlyWindowsDeviceGroup => WS_DENY_ONLY_WINDOWS_DEVICE_GROUP,
            #[cfg(feature = "ws-claims")]
            Self::WsDsa => WS_DSA,
            #[cfg(feature = "ws-claims")]
            Self::WsExpiration => WS_EXPIRATION,
            #[cfg(feature = "ws-claims")]
            Self::WsExpired => WS_EXPIRED,
            #[cfg(feature = "ws-claims")]
            Self::WsGroupSid => WS_GROUP_SID,
            #[cfg(feature = "ws-claims")]
            Self::WsIsPersistent => WS_IS_PERSISTENT,
            #[cfg(feature = "ws-claims")]
            Self::WsPrimaryGroupSid => WS_PRIMARY_GROUP_SID,
            #[cfg(feature = "ws-claims")]
            Self::WsPrimarySid => WS_PRIMARY_SID,
            #[cfg(feature = "ws-claims")]
            Self::WsRole => WS_ROLE,
            #[cfg(feature = "ws-claims")]
            Self::WsSerialNumber => WS_SERIAL_NUMBER,
            #[cfg(feature = "ws-claims")]
            Self::WsUserData => WS_USER_DATA,
            #[cfg(feature = "ws-claims")]
            Self::WsVersion => WS_VERSION,
            #[cfg(feature = "ws-claims")]
            Self::WsWindowsAccountName => WS_WINDOWS_ACCOUNT_NAME,
            #[cfg(feature = "ws-claims")]
            Self::WsWindowsDeviceClaim => WS_WINDOWS_DEVICE_CLAIM,
            #[cfg(feature = "ws-claims")]
            Self::WsWindowsDeviceGroup => WS_WINDOWS_DEVICE_GROUP,
            #[cfg(feature = "ws-claims")]
            Self::WsWindowsUserClaim => WS_WINDOWS_USER_CLAIM,
            #[cfg(feature = "ws-claims")]
            Self::WsWindowsFqbnVersion => WS_WINDOWS_FQBN_VERSION,
            #[cfg(feature = "ws-claims")]
            Self::WsWindowsSubAuthority => WS_WINDOWS_SUB_AUTHORITY,
            #[cfg(feature = "ws-claims")]
            Self::WsAnonymous => WS_ANONYMOUS,
            #[cfg(feature = "ws-claims")]
            Self::WsAuthentication => WS_AUTHENTICATION,
            #[cfg(feature = "ws-claims")]
            Self::WsAuthorizationDecision => WS_AUTHORIZATION_DECISION,
            #[cfg(feature = "ws-claims")]
            Self::WsCountry => WS_COUNTRY,
            #[cfg(feature = "ws-claims")]
            Self::WsDateOfBirth => WS_DATE_OF_BIRTH,
            #[cfg(feature = "ws-claims")]
            Self::WsDns => WS_DNS,
            #[cfg(feature = "ws-claims")]
            Self::WsDenyOnlySid => WS_DENY_ONLY_SID,
            #[cfg(feature = "ws-claims")]
            Self::WsEmail => WS_EMAIL,
            #[cfg(feature = "ws-claims")]
            Self::WsGender => WS_GENDER,
            #[cfg(feature = "ws-claims")]
            Self::WsGivenName => WS_GIVEN_NAME,
            #[cfg(feature = "ws-claims")]
            Self::WsHash => WS_HASH,
            #[cfg(feature = "ws-claims")]
            Self::WsHomePhone => WS_HOME_PHONE,
            #[cfg(feature = "ws-claims")]
            Self::WsLocality => WS_LOCALITY,
            #[cfg(feature = "ws-claims")]
            Self::WsMobilePhone => WS_MOBILE_PHONE,
            #[cfg(feature = "ws-claims")]
            Self::WsName => WS_NAME,
            #[cfg(feature = "ws-claims")]
            Self::WsNameIdentifier => WS_NAME_IDENTIFIER,
            #[cfg(feature = "ws-claims")]
            Self::WsOtherPhone => WS_OTHER_PHONE,
            #[cfg(feature = "ws-claims")]
            Self::WsPostalCode => WS_POSTAL_CODE,
            #[cfg(feature = "ws-claims")]
            Self::WsRsa => WS_RSA,
            #[cfg(feature = "ws-claims")]
            Self::WsSid => WS_SID,
            #[cfg(feature = "ws-claims")]
            Self::WsSpn => WS_SPN,
            #[cfg(feature = "ws-claims")]
            Self::WsStateOrProvince => WS_STATE_OR_PROVINCE,
            #[cfg(feature = "ws-claims")]
            Self::WsStreetAddress => WS_STREET_ADDRESS,
            #[cfg(feature = "ws-claims")]
            Self::WsSurname => WS_SURNAME,
            #[cfg(feature = "ws-claims")]
            Self::WsSystem => WS_SYSTEM,
            #[cfg(feature = "ws-claims")]
            Self::WsThumbprint => WS_THUMBPRINT,
            #[cfg(feature = "ws-claims")]
            Self::WsUpn => WS_UPN,
            #[cfg(feature = "ws-claims")]
            Self::WsUri => WS_URI,
            #[cfg(feature = "ws-claims")]
            Self::WsWebpage => WS_WEBPAGE,
            #[cfg(feature = "ws-claims")]
            Self::WsX500DistinguishedName => WS_X500_DISTINGUISHED_NAME,
            #[cfg(feature = "ws-claims")]
            Self::WsActor => WS_ACTOR,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            CLAIM_ISS => Some(Self::Iss),
            CLAIM_SUB => Some(Self::Sub),
            CLAIM_AUD => Some(Self::Aud),
            CLAIM_EXP => Some(Self::Exp),
            CLAIM_NBF => Some(Self::Nbf),
            CLAIM_IAT => Some(Self::Iat),
            CLAIM_JTI => Some(Self::Jti),
            CLAIM_ACT => Some(Self::Act),
            CLAIM_MAY_ACT => Some(Self::MayAct),
            CLAIM_CNF => Some(Self::Cnf),
            CLAIM_SCOPE => Some(Self::Scope),
            CLAIM_CLIENT_ID => Some(Self::ClientId),
            CLAIM_AUTH_TIME => Some(Self::AuthTime),
            CLAIM_ACR => Some(Self::Acr),
            CLAIM_AMR => Some(Self::Amr),
            CLAIM_AZP => Some(Self::Azp),
            CLAIM_NONCE => Some(Self::Nonce),
            CLAIM_NAME => Some(Self::Name),
            CLAIM_EMAIL => Some(Self::Email),
            CLAIM_EMAIL_VERIFIED => Some(Self::EmailVerified),
            CLAIM_PREFERRED_USERNAME => Some(Self::PreferredUsername),
            CLAIM_GIVEN_NAME => Some(Self::GivenName),
            CLAIM_FAMILY_NAME => Some(Self::FamilyName),
            CLAIM_ROLE => Some(Self::Role),
            CLAIM_ROLES => Some(Self::Roles),
            CLAIM_GROUPS => Some(Self::Groups),
            CLAIM_UPN => Some(Self::Upn),
            #[cfg(feature = "ws-claims")]
            WS_AUTHENTICATION_INSTANT => Some(Self::WsAuthenticationInstant),
            #[cfg(feature = "ws-claims")]
            WS_AUTHENTICATION_METHOD => Some(Self::WsAuthenticationMethod),
            #[cfg(feature = "ws-claims")]
            WS_COOKIE_PATH => Some(Self::WsCookiePath),
            #[cfg(feature = "ws-claims")]
            WS_DENY_ONLY_PRIMARY_SID => Some(Self::WsDenyOnlyPrimarySid),
            #[cfg(feature = "ws-claims")]
            WS_DENY_ONLY_PRIMARY_GROUP_SID => Some(Self::WsDenyOnlyPrimaryGroupSid),
            #[cfg(feature = "ws-claims")]
            WS_DENY_ONLY_WINDOWS_DEVICE_GROUP => Some(Self::WsDenyOnlyWindowsDeviceGroup),
            #[cfg(feature = "ws-claims")]
            WS_DSA => Some(Self::WsDsa),
            #[cfg(feature = "ws-claims")]
            WS_EXPIRATION => Some(Self::WsExpiration),
            #[cfg(feature = "ws-claims")]
            WS_EXPIRED => Some(Self::WsExpired),
            #[cfg(feature = "ws-claims")]
            WS_GROUP_SID => Some(Self::WsGroupSid),
            #[cfg(feature = "ws-claims")]
            WS_IS_PERSISTENT => Some(Self::WsIsPersistent),
            #[cfg(feature = "ws-claims")]
            WS_PRIMARY_GROUP_SID => Some(Self::WsPrimaryGroupSid),
            #[cfg(feature = "ws-claims")]
            WS_PRIMARY_SID => Some(Self::WsPrimarySid),
            #[cfg(feature = "ws-claims")]
            WS_ROLE => Some(Self::WsRole),
            #[cfg(feature = "ws-claims")]
            WS_SERIAL_NUMBER => Some(Self::WsSerialNumber),
            #[cfg(feature = "ws-claims")]
            WS_USER_DATA => Some(Self::WsUserData),
            #[cfg(feature = "ws-claims")]
            WS_VERSION => Some(Self::WsVersion),
            #[cfg(feature = "ws-claims")]
            WS_WINDOWS_ACCOUNT_NAME => Some(Self::WsWindowsAccountName),
            #[cfg(feature = "ws-claims")]
            WS_WINDOWS_DEVICE_CLAIM => Some(Self::WsWindowsDeviceClaim),
            #[cfg(feature = "ws-claims")]
            WS_WINDOWS_DEVICE_GROUP => Some(Self::WsWindowsDeviceGroup),
            #[cfg(feature = "ws-claims")]
            WS_WINDOWS_USER_CLAIM => Some(Self::WsWindowsUserClaim),
            #[cfg(feature = "ws-claims")]
            WS_WINDOWS_FQBN_VERSION => Some(Self::WsWindowsFqbnVersion),
            #[cfg(feature = "ws-claims")]
            WS_WINDOWS_SUB_AUTHORITY => Some(Self::WsWindowsSubAuthority),
            #[cfg(feature = "ws-claims")]
            WS_ANONYMOUS => Some(Self::WsAnonymous),
            #[cfg(feature = "ws-claims")]
            WS_AUTHENTICATION => Some(Self::WsAuthentication),
            #[cfg(feature = "ws-claims")]
            WS_AUTHORIZATION_DECISION => Some(Self::WsAuthorizationDecision),
            #[cfg(feature = "ws-claims")]
            WS_COUNTRY => Some(Self::WsCountry),
            #[cfg(feature = "ws-claims")]
            WS_DATE_OF_BIRTH => Some(Self::WsDateOfBirth),
            #[cfg(feature = "ws-claims")]
            WS_DNS => Some(Self::WsDns),
            #[cfg(feature = "ws-claims")]
            WS_DENY_ONLY_SID => Some(Self::WsDenyOnlySid),
            #[cfg(feature = "ws-claims")]
            WS_EMAIL => Some(Self::WsEmail),
            #[cfg(feature = "ws-claims")]
            WS_GENDER => Some(Self::WsGender),
            #[cfg(feature = "ws-claims")]
            WS_GIVEN_NAME => Some(Self::WsGivenName),
            #[cfg(feature = "ws-claims")]
            WS_HASH => Some(Self::WsHash),
            #[cfg(feature = "ws-claims")]
            WS_HOME_PHONE => Some(Self::WsHomePhone),
            #[cfg(feature = "ws-claims")]
            WS_LOCALITY => Some(Self::WsLocality),
            #[cfg(feature = "ws-claims")]
            WS_MOBILE_PHONE => Some(Self::WsMobilePhone),
            #[cfg(feature = "ws-claims")]
            WS_NAME => Some(Self::WsName),
            #[cfg(feature = "ws-claims")]
            WS_NAME_IDENTIFIER => Some(Self::WsNameIdentifier),
            #[cfg(feature = "ws-claims")]
            WS_OTHER_PHONE => Some(Self::WsOtherPhone),
            #[cfg(feature = "ws-claims")]
            WS_POSTAL_CODE => Some(Self::WsPostalCode),
            #[cfg(feature = "ws-claims")]
            WS_RSA => Some(Self::WsRsa),
            #[cfg(feature = "ws-claims")]
            WS_SID => Some(Self::WsSid),
            #[cfg(feature = "ws-claims")]
            WS_SPN => Some(Self::WsSpn),
            #[cfg(feature = "ws-claims")]
            WS_STATE_OR_PROVINCE => Some(Self::WsStateOrProvince),
            #[cfg(feature = "ws-claims")]
            WS_STREET_ADDRESS => Some(Self::WsStreetAddress),
            #[cfg(feature = "ws-claims")]
            WS_SURNAME => Some(Self::WsSurname),
            #[cfg(feature = "ws-claims")]
            WS_SYSTEM => Some(Self::WsSystem),
            #[cfg(feature = "ws-claims")]
            WS_THUMBPRINT => Some(Self::WsThumbprint),
            #[cfg(feature = "ws-claims")]
            WS_UPN => Some(Self::WsUpn),
            #[cfg(feature = "ws-claims")]
            WS_URI => Some(Self::WsUri),
            #[cfg(feature = "ws-claims")]
            WS_WEBPAGE => Some(Self::WsWebpage),
            #[cfg(feature = "ws-claims")]
            WS_X500_DISTINGUISHED_NAME => Some(Self::WsX500DistinguishedName),
            #[cfg(feature = "ws-claims")]
            WS_ACTOR => Some(Self::WsActor),
            _ => None,
        }
    }
}
