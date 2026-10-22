//! End-to-end tests for the presence service behind the NATS auth callout.
//!
//! The crate has no library code. It exists because the auth crate already depends on the
//! service crate, so the service crate cannot take the auth crate as a dev-dependency without
//! a cycle that would duplicate every shared type. The async-nats `websockets` feature is
//! enabled only here, on a dev-dependency, so no library crate ships it.
