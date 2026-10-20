use crate::claim_property_key::ClaimPropertyKey;
use crate::claim_text::ClaimText;
use crate::confirmation::Confirmation;
use crate::constants::{
    JWK_PRIVATE_MEMBERS, KTY_OCT, MEMBER_ALG, MEMBER_CRV, MEMBER_JKT, MEMBER_JKU, MEMBER_JWE, MEMBER_JWK, MEMBER_KID,
    MEMBER_KTY, MEMBER_USE, MEMBER_X5T_S256,
};
use crate::ingest_error::ConfirmationError;
use crate::jwk::Jwk;
use crate::raw_claims::RawClaims;
use crate::raw_claims_error::RawClaimsError;
use crate::raw_json::RawJson;

fn object_of(raw: &RawJson, member: &'static str) -> Result<RawClaims, ConfirmationError> {
    RawClaims::from_object(raw).map_err(|error| match error {
        RawClaimsError::NotObject => ConfirmationError::WrongType { member },
        other => ConfirmationError::Malformed(other),
    })
}

fn text_of(raw: &RawJson, member: &'static str) -> Result<ClaimText, ConfirmationError> {
    raw.as_string()
        .map(ClaimText::from)
        .ok_or(ConfirmationError::WrongType { member })
}

fn parse_jwk(raw: &RawJson) -> Result<Jwk, ConfirmationError> {
    let object = object_of(raw, MEMBER_JWK)?;
    let mut kty = None;
    let mut kid = None;
    let mut alg = None;
    let mut key_use = None;
    let mut crv = None;
    let mut members = Vec::new();
    for (name, value) in object.iter() {
        let name = name.as_str();
        if JWK_PRIVATE_MEMBERS.contains(&name) {
            return Err(ConfirmationError::PrivateKeyMaterial);
        }
        match name {
            MEMBER_KTY => kty = Some(text_of(value, MEMBER_KTY)?),
            MEMBER_KID => kid = Some(text_of(value, MEMBER_KID)?),
            MEMBER_ALG => alg = Some(text_of(value, MEMBER_ALG)?),
            MEMBER_USE => key_use = Some(text_of(value, MEMBER_USE)?),
            MEMBER_CRV => crv = Some(text_of(value, MEMBER_CRV)?),
            other => members.push((
                ClaimPropertyKey::try_new(other.to_owned())?,
                ClaimText::from(value.as_str().to_owned()),
            )),
        }
    }
    let kty = kty.ok_or(ConfirmationError::MissingKeyType)?;
    if kty.as_str() == KTY_OCT {
        return Err(ConfirmationError::SymmetricKey);
    }
    Ok(Jwk {
        kty,
        kid,
        alg,
        key_use,
        crv,
        members,
    })
}

pub(crate) fn parse_confirmation(raw: &RawJson) -> Result<Confirmation, ConfirmationError> {
    let object = object_of(raw, "cnf")?;
    if object.is_empty() {
        return Err(ConfirmationError::Empty);
    }
    let mut confirmation = Confirmation {
        jwk: None,
        jwe: None,
        jku: None,
        kid: None,
        x5t_s256: None,
        jkt: None,
        other: Vec::new(),
    };
    let mut key_members = 0;
    for (name, value) in object.iter() {
        match name.as_str() {
            MEMBER_JWK => {
                key_members += 1;
                confirmation.jwk = Some(parse_jwk(value)?);
            }
            MEMBER_JWE => {
                key_members += 1;
                confirmation.jwe = Some(text_of(value, MEMBER_JWE)?);
            }
            MEMBER_JKU => {
                key_members += 1;
                confirmation.jku = Some(text_of(value, MEMBER_JKU)?);
            }
            MEMBER_KID => confirmation.kid = Some(text_of(value, MEMBER_KID)?),
            MEMBER_X5T_S256 => confirmation.x5t_s256 = Some(text_of(value, MEMBER_X5T_S256)?),
            MEMBER_JKT => confirmation.jkt = Some(text_of(value, MEMBER_JKT)?),
            other => confirmation.other.push((
                ClaimPropertyKey::try_new(other.to_owned())?,
                ClaimText::from(value.as_str().to_owned()),
            )),
        }
    }
    if key_members > 1 {
        return Err(ConfirmationError::MultipleKeyMembers);
    }
    Ok(confirmation)
}
