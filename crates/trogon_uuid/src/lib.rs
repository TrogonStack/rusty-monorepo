#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

pub use uuid::Uuid;

/// A compiled identity recipe with a resolved, version-specific namespace.
pub trait UuidTemplate: 'static {
    type Input<'a>;
    const NAMESPACE: Uuid;
    fn uuid5(input: Self::Input<'_>) -> Uuid;
}

/// A namespace specification using exact UTF-8 text for DNS and URL names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace<'a> {
    Uuid(Uuid),
    Dns(&'a str),
    Url(&'a str),
}

impl Namespace<'_> {
    pub fn resolve(self) -> Uuid {
        Uuid::nil()
    }

    pub fn version_namespace(self, _version_name: &str) -> Uuid {
        Uuid::nil()
    }
}

/// Derives a UUIDv5 from one name without allocating.
pub fn uuid5(_namespace: &Uuid, _name: &[u8]) -> Uuid {
    Uuid::nil()
}

/// Derives a UUIDv5 from ordered byte pieces without concatenating them.
pub fn uuid5_parts<I, P>(_namespace: &Uuid, _parts: I) -> Uuid
where
    I: IntoIterator<Item = P>,
    P: AsRef<[u8]>,
{
    Uuid::nil()
}
