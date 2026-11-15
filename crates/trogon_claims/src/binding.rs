use crate::claim_text::ClaimText;
use crate::confirmation::Confirmation;
use crate::verified_binding::{BindingError, BindingMethod, Method, VerifiedBinding};

/// Sender-constraint state of a token. [`Self::Asserted`] carries `cnf` from
/// [RFC 7800](https://www.rfc-editor.org/rfc/rfc7800.html); [`Self::Verified`]
/// records a thumbprint match against evidence supplied by the verifying layer.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Binding {
    Bearer,
    Asserted(Box<Confirmation>),
    Verified(Box<VerifiedBinding>),
}

impl Binding {
    /// Compares the asserted thumbprint with one supplied by the caller.
    ///
    /// The caller must first validate the DPoP proof under
    /// [RFC 9449 §4.3](https://www.rfc-editor.org/rfc/rfc9449.html#section-4.3)
    /// or obtain the client certificate through authenticated mutual TLS under
    /// [RFC 8705 §3](https://www.rfc-editor.org/rfc/rfc8705.html#section-3).
    /// This method performs no signature, request-binding or transport checks.
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
