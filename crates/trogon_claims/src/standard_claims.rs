use crate::audience::Audience;
use crate::authentication_method::AuthenticationMethod;
use crate::claim_lookup_error::ClaimLookupError;
use crate::claim_text::ClaimText;
use crate::claim_value::ClaimValue;
use crate::client_id::ClientId;
#[cfg(feature = "json")]
use crate::dynamic_claims::ClaimShape;
use crate::dynamic_claims::DynamicClaims;
use crate::group_name::GroupName;
use crate::issuer::Issuer;
use crate::numeric_date::NumericDate;
use crate::role_name::RoleName;
use crate::scope::ScopeSet;
use crate::subject_id::SubjectId;
use crate::well_known_claim::WellKnownClaim;

/// Typed interpretation of standard claim assertions. Requiredness and token
/// acceptance remain caller policy.
///
/// JSON ingest retains member shapes: `roles`, `groups` and `amr` require arrays,
/// while `aud` accepts a string or an array. For manually constructed
/// [`DynamicClaims::from_claims`] values, a collection can be a single
/// [`ClaimValue::List`] or repeated textual claims, matching flat projection.
/// Scalar accessors reject multiple entries instead of selecting one.
#[derive(Clone, Copy, Debug)]
pub struct StandardClaims<'a> {
    pub(crate) claims: &'a DynamicClaims,
}

/// Standard claim values parsed once, with infallible access to the resulting
/// assertions. Requiredness and token acceptance remain caller policy.
/// Generic identifiers retain their core contracts; this is not a JWT profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedStandardClaims<'a> {
    issuer: Option<Issuer>,
    subject: Option<SubjectId>,
    preferred_username: Option<&'a ClaimText>,
    upn: Option<&'a ClaimText>,
    display_name: Option<&'a ClaimText>,
    email: Option<&'a ClaimText>,
    email_verified: Option<bool>,
    audiences: Option<Vec<Audience>>,
    expires_at: Option<NumericDate>,
    issued_at: Option<NumericDate>,
    not_before: Option<NumericDate>,
    authenticated_at: Option<NumericDate>,
    authentication_methods: Option<Vec<AuthenticationMethod>>,
    client_id: Option<ClientId>,
    scopes: Option<ScopeSet>,
    roles: Option<Vec<RoleName>>,
    groups: Option<Vec<GroupName>>,
}

impl<'a> StandardClaims<'a> {
    pub fn try_parse(self) -> Result<ParsedStandardClaims<'a>, ClaimLookupError> {
        Ok(ParsedStandardClaims {
            issuer: self.issuer()?,
            subject: self.subject()?,
            preferred_username: self.preferred_username()?,
            upn: self.upn()?,
            display_name: self.display_name()?,
            email: self.email()?,
            email_verified: self.email_verified()?,
            audiences: self.audiences()?,
            expires_at: self.expires_at()?,
            issued_at: self.issued_at()?,
            not_before: self.not_before()?,
            authenticated_at: self.authenticated_at()?,
            authentication_methods: self.authentication_methods()?,
            client_id: self.client_id()?,
            scopes: self.scopes()?,
            roles: self.roles()?,
            groups: self.groups()?,
        })
    }

    fn projection_check(&self, claim: WellKnownClaim) -> Result<(), ClaimLookupError> {
        if self
            .claims
            .unprojected()
            .iter()
            .any(|name| name.well_known() == Some(claim))
        {
            return Err(ClaimLookupError::WrongType { claim });
        }
        Ok(())
    }

    fn is_source_array(&self, claim: WellKnownClaim) -> bool {
        #[cfg(feature = "json")]
        {
            self.claims
                .source_shapes
                .iter()
                .any(|(name, shape)| name.well_known() == Some(claim) && *shape == ClaimShape::Array)
        }
        #[cfg(not(feature = "json"))]
        {
            let _ = claim;
            false
        }
    }

    fn is_source_scalar(&self, claim: WellKnownClaim) -> bool {
        #[cfg(feature = "json")]
        {
            self.claims
                .source_shapes
                .iter()
                .any(|(name, shape)| name.well_known() == Some(claim) && *shape == ClaimShape::Scalar)
        }
        #[cfg(not(feature = "json"))]
        {
            let _ = claim;
            false
        }
    }

    fn single(&self, claim: WellKnownClaim) -> Result<Option<&'a ClaimValue>, ClaimLookupError> {
        self.projection_check(claim)?;
        if self.is_source_array(claim) {
            return Err(ClaimLookupError::WrongType { claim });
        }
        let mut values = self
            .claims
            .claims()
            .iter()
            .filter(|value| value.claim_type().well_known() == Some(claim));
        let value = values.next().map(|value| value.value());
        if values.next().is_some() {
            return Err(ClaimLookupError::MultipleValues { claim });
        }
        Ok(value)
    }

    fn collection<T>(
        &self,
        claim: WellKnownClaim,
        allow_scalar: bool,
        convert: impl Fn(&ClaimValue) -> Result<T, ClaimLookupError>,
    ) -> Result<Option<Vec<T>>, ClaimLookupError> {
        self.projection_check(claim)?;
        if self.is_source_scalar(claim) && !allow_scalar {
            return Err(ClaimLookupError::WrongType { claim });
        }
        let values: Vec<_> = self
            .claims
            .claims()
            .iter()
            .filter(|value| value.claim_type().well_known() == Some(claim))
            .map(|value| value.value())
            .collect();
        if values.is_empty() {
            return Ok(self.is_source_array(claim).then(Vec::new));
        }
        if let [ClaimValue::List(items)] = values.as_slice() {
            return items.iter().map(convert).collect::<Result<Vec<_>, _>>().map(Some);
        }
        values.into_iter().map(convert).collect::<Result<Vec<_>, _>>().map(Some)
    }

    /// Reads a scalar textual claim without coercion or fallback.
    pub fn text(&self, claim: WellKnownClaim) -> Result<Option<&'a ClaimText>, ClaimLookupError> {
        self.single(claim)?
            .map(|value| match value {
                ClaimValue::Text(text) => Ok(text),
                _ => Err(ClaimLookupError::WrongType { claim }),
            })
            .transpose()
    }

    pub fn issuer(&self) -> Result<Option<Issuer>, ClaimLookupError> {
        self.text(WellKnownClaim::Iss)?
            .map(|text| {
                Issuer::try_new(text.as_str().to_owned()).map_err(|_| ClaimLookupError::InvalidValue {
                    claim: WellKnownClaim::Iss,
                })
            })
            .transpose()
    }

    pub fn subject(&self) -> Result<Option<SubjectId>, ClaimLookupError> {
        self.text(WellKnownClaim::Sub)?
            .map(|text| {
                SubjectId::try_new(text.as_str().to_owned()).map_err(|_| ClaimLookupError::InvalidValue {
                    claim: WellKnownClaim::Sub,
                })
            })
            .transpose()
    }

    pub fn preferred_username(&self) -> Result<Option<&'a ClaimText>, ClaimLookupError> {
        self.text(WellKnownClaim::PreferredUsername)
    }

    pub fn upn(&self) -> Result<Option<&'a ClaimText>, ClaimLookupError> {
        self.text(WellKnownClaim::Upn)
    }

    pub fn display_name(&self) -> Result<Option<&'a ClaimText>, ClaimLookupError> {
        self.text(WellKnownClaim::Name)
    }

    pub fn email(&self) -> Result<Option<&'a ClaimText>, ClaimLookupError> {
        self.text(WellKnownClaim::Email)
    }

    pub fn email_verified(&self) -> Result<Option<bool>, ClaimLookupError> {
        self.single(WellKnownClaim::EmailVerified)?
            .map(|value| match value {
                ClaimValue::Boolean(value) => Ok(*value),
                _ => Err(ClaimLookupError::WrongType {
                    claim: WellKnownClaim::EmailVerified,
                }),
            })
            .transpose()
    }

    pub fn audiences(&self) -> Result<Option<Vec<Audience>>, ClaimLookupError> {
        self.collection(WellKnownClaim::Aud, true, |value| match value {
            ClaimValue::Text(text) => {
                Audience::try_new(text.as_str().to_owned()).map_err(|_| ClaimLookupError::InvalidValue {
                    claim: WellKnownClaim::Aud,
                })
            }
            _ => Err(ClaimLookupError::WrongType {
                claim: WellKnownClaim::Aud,
            }),
        })
    }

    fn numeric_date(&self, claim: WellKnownClaim) -> Result<Option<NumericDate>, ClaimLookupError> {
        self.single(claim)?
            .map(|value| match value {
                ClaimValue::Timestamp(timestamp) => NumericDate::try_new(timestamp.secs(), timestamp.nanos())
                    .map_err(|_| ClaimLookupError::InvalidValue { claim }),
                ClaimValue::Integer(secs) => {
                    NumericDate::try_new(*secs, 0).map_err(|_| ClaimLookupError::InvalidValue { claim })
                }
                ClaimValue::Unsigned(secs) => NumericDate::try_from_lexical(&secs.get().to_string())
                    .map_err(|_| ClaimLookupError::InvalidValue { claim }),
                ClaimValue::Double(value) if value.is_finite() => NumericDate::try_from_lexical(&value.to_string())
                    .map_err(|_| ClaimLookupError::InvalidValue { claim }),
                _ => Err(ClaimLookupError::WrongType { claim }),
            })
            .transpose()
    }

    pub fn expires_at(&self) -> Result<Option<NumericDate>, ClaimLookupError> {
        self.numeric_date(WellKnownClaim::Exp)
    }

    pub fn issued_at(&self) -> Result<Option<NumericDate>, ClaimLookupError> {
        self.numeric_date(WellKnownClaim::Iat)
    }

    pub fn not_before(&self) -> Result<Option<NumericDate>, ClaimLookupError> {
        self.numeric_date(WellKnownClaim::Nbf)
    }

    pub fn authenticated_at(&self) -> Result<Option<NumericDate>, ClaimLookupError> {
        self.numeric_date(WellKnownClaim::AuthTime)
    }

    pub fn authentication_methods(&self) -> Result<Option<Vec<AuthenticationMethod>>, ClaimLookupError> {
        self.collection(WellKnownClaim::Amr, false, |value| match value {
            ClaimValue::Text(text) if !text.as_str().is_empty() => {
                Ok(AuthenticationMethod::from_value(text.as_str().to_owned()))
            }
            ClaimValue::Text(_) => Err(ClaimLookupError::InvalidValue {
                claim: WellKnownClaim::Amr,
            }),
            _ => Err(ClaimLookupError::WrongType {
                claim: WellKnownClaim::Amr,
            }),
        })
    }

    pub fn client_id(&self) -> Result<Option<ClientId>, ClaimLookupError> {
        self.text(WellKnownClaim::ClientId)?
            .map(|text| {
                ClientId::try_new(text.as_str().to_owned()).map_err(|_| ClaimLookupError::InvalidValue {
                    claim: WellKnownClaim::ClientId,
                })
            })
            .transpose()
    }

    pub fn scopes(&self) -> Result<Option<ScopeSet>, ClaimLookupError> {
        self.text(WellKnownClaim::Scope)?
            .map(|text| {
                ScopeSet::try_from_space_delimited(text.as_str()).map_err(|_| ClaimLookupError::InvalidValue {
                    claim: WellKnownClaim::Scope,
                })
            })
            .transpose()
    }

    pub fn roles(&self) -> Result<Option<Vec<RoleName>>, ClaimLookupError> {
        self.collection(WellKnownClaim::Roles, false, |value| match value {
            ClaimValue::Text(text) => {
                RoleName::try_new(text.as_str().to_owned()).map_err(|_| ClaimLookupError::InvalidValue {
                    claim: WellKnownClaim::Roles,
                })
            }
            _ => Err(ClaimLookupError::WrongType {
                claim: WellKnownClaim::Roles,
            }),
        })
    }

    pub fn groups(&self) -> Result<Option<Vec<GroupName>>, ClaimLookupError> {
        self.collection(WellKnownClaim::Groups, false, |value| match value {
            ClaimValue::Text(text) => {
                GroupName::try_new(text.as_str().to_owned()).map_err(|_| ClaimLookupError::InvalidValue {
                    claim: WellKnownClaim::Groups,
                })
            }
            _ => Err(ClaimLookupError::WrongType {
                claim: WellKnownClaim::Groups,
            }),
        })
    }
}

impl<'a> TryFrom<&'a DynamicClaims> for ParsedStandardClaims<'a> {
    type Error = ClaimLookupError;

    fn try_from(claims: &'a DynamicClaims) -> Result<Self, Self::Error> {
        claims.standard().try_parse()
    }
}

impl<'a> ParsedStandardClaims<'a> {
    pub fn issuer(&self) -> Option<&Issuer> {
        self.issuer.as_ref()
    }

    pub fn subject(&self) -> Option<&SubjectId> {
        self.subject.as_ref()
    }

    pub fn preferred_username(&self) -> Option<&'a ClaimText> {
        self.preferred_username
    }

    pub fn upn(&self) -> Option<&'a ClaimText> {
        self.upn
    }

    pub fn display_name(&self) -> Option<&'a ClaimText> {
        self.display_name
    }

    pub fn email(&self) -> Option<&'a ClaimText> {
        self.email
    }

    pub fn email_verified(&self) -> Option<bool> {
        self.email_verified
    }

    pub fn audiences(&self) -> Option<&[Audience]> {
        self.audiences.as_deref()
    }

    pub fn expires_at(&self) -> Option<NumericDate> {
        self.expires_at
    }

    pub fn issued_at(&self) -> Option<NumericDate> {
        self.issued_at
    }

    pub fn not_before(&self) -> Option<NumericDate> {
        self.not_before
    }

    pub fn authenticated_at(&self) -> Option<NumericDate> {
        self.authenticated_at
    }

    pub fn authentication_methods(&self) -> Option<&[AuthenticationMethod]> {
        self.authentication_methods.as_deref()
    }

    pub fn client_id(&self) -> Option<&ClientId> {
        self.client_id.as_ref()
    }

    pub fn scopes(&self) -> Option<&ScopeSet> {
        self.scopes.as_ref()
    }

    pub fn roles(&self) -> Option<&[RoleName]> {
        self.roles.as_deref()
    }

    pub fn groups(&self) -> Option<&[GroupName]> {
        self.groups.as_deref()
    }
}
