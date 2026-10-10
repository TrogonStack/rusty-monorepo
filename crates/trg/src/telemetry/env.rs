//! Where telemetry reads its configuration variables from.
//!
//! Production reads the process environment through [`ProcessEnv`]. Tests
//! hand in a closure instead, so they never mutate the process environment,
//! which races with every other test running in parallel.

/// A source of environment variables, keyed by name.
pub trait EnvLookup {
    fn get(&self, key: &str) -> Option<String>;
}

impl<F> EnvLookup for F
where
    F: Fn(&str) -> Option<String>,
{
    fn get(&self, key: &str) -> Option<String> {
        self(key)
    }
}

/// The real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvLookup for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

#[cfg(test)]
pub(crate) fn fixed(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |key| {
        pairs
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| (*value).to_string())
    }
}
