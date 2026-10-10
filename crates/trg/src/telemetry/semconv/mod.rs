//! Attribute, metric and span names used by `trg`'s telemetry.
//!
//! [`generated`] comes from Weaver, pinned to a commit of
//! <https://github.com/open-telemetry/semantic-conventions-genai> (see that
//! module for the exact commit). [`trg`] is hand-written: it names concepts
//! `trg` owns and no semantic convention covers. The two are kept in
//! separate modules so a reader never has to guess which is which.
//!
//! Non-GenAI names (`process.*`, `service.*`, `error.type`, ...) come from
//! the `opentelemetry-semantic-conventions` crate directly rather than from
//! here.

pub mod generated;
pub mod trg;
