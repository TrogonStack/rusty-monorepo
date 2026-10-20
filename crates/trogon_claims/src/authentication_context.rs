use crate::authentication_method::AuthenticationMethod;
use crate::claim_text::ClaimText;
use crate::numeric_date::NumericDate;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AuthenticationContext {
    acr: Option<ClaimText>,
    amr: Vec<AuthenticationMethod>,
    auth_time: Option<NumericDate>,
}

impl AuthenticationContext {
    pub fn new(acr: Option<ClaimText>, amr: Vec<AuthenticationMethod>, auth_time: Option<NumericDate>) -> Self {
        Self { acr, amr, auth_time }
    }

    pub fn acr(&self) -> Option<&ClaimText> {
        self.acr.as_ref()
    }

    pub fn amr(&self) -> &[AuthenticationMethod] {
        &self.amr
    }

    pub fn auth_time(&self) -> Option<NumericDate> {
        self.auth_time
    }
}
