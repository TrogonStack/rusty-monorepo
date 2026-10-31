use crate::claim_text::ClaimText;
use crate::confirmation::Confirmation;
use crate::verified_binding::{BindingError, BindingMethod, Method, VerifiedBinding};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Binding {
    Bearer,
    Asserted(Box<Confirmation>),
    Verified(Box<VerifiedBinding>),
}

impl Binding {
    pub fn verify<M: Method>(&self, presented: &str) -> Result<VerifiedBinding, BindingError> {
        let method = M::METHOD;
        let asserted = match self {
            Self::Bearer => return Err(BindingError::NotAsserted),
            Self::Verified(verified) => {
                return if verified.method() == method && verified.thumbprint() == presented {
                    Ok(verified.as_ref().clone())
                } else {
                    Err(BindingError::Mismatch(method))
                };
            }
            Self::Asserted(confirmation) => match method {
                BindingMethod::Dpop => confirmation.jkt(),
                BindingMethod::MutualTls => confirmation.x5t_s256(),
            },
        };
        let asserted: &ClaimText = asserted.ok_or(BindingError::MemberMissing(method))?;
        if asserted.as_str() == presented {
            Ok(VerifiedBinding::new(method, asserted.clone()))
        } else {
            Err(BindingError::Mismatch(method))
        }
    }
}
