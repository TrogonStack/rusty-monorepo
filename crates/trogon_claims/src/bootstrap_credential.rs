use std::fmt;

use zeroize::ZeroizeOnDrop;

pub struct BootstrapCredential(Box<str>);

impl BootstrapCredential {
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    pub fn expose_for_token_exchange(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BootstrapCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BootstrapCredential(<redacted>)")
    }
}

impl Drop for BootstrapCredential {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut std::mem::take(&mut self.0).into_boxed_bytes());
    }
}

impl ZeroizeOnDrop for BootstrapCredential {}
