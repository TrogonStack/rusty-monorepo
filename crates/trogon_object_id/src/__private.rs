#[cfg(feature = "alloc")]
pub use alloc::string::String;
#[cfg(feature = "serde")]
pub use serde;
#[cfg(all(feature = "serde", not(feature = "alloc")))]
pub use serde_support::deserialize;
#[cfg(all(feature = "serde", feature = "alloc"))]
pub use serde_support::deserialize_owned;
#[cfg(feature = "serde")]
pub use serde_support::serialize;
#[cfg(feature = "serde")]
mod serde_support;
