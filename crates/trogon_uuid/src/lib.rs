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
        match self {
            Self::Uuid(namespace) => namespace,
            Self::Dns(name) => uuid5(&Uuid::NAMESPACE_DNS, name.as_bytes()),
            Self::Url(name) => uuid5(&Uuid::NAMESPACE_URL, name.as_bytes()),
        }
    }

    pub fn version_namespace(self, version_name: &str) -> Uuid {
        uuid5(&self.resolve(), version_name.as_bytes())
    }
}

/// Derives a UUIDv5 from one name without allocating.
pub fn uuid5(namespace: &Uuid, name: &[u8]) -> Uuid {
    Uuid::new_v5(namespace, name)
}

/// Derives a UUIDv5 from ordered byte pieces without concatenating them.
pub fn uuid5_parts<I, P>(namespace: &Uuid, parts: I) -> Uuid
where
    I: IntoIterator<Item = P>,
    P: AsRef<[u8]>,
{
    let mut hasher = sha1_smol::Sha1::new();
    hasher.update(namespace.as_bytes());
    for part in parts {
        hasher.update(part.as_ref());
    }
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hasher.digest().bytes()[..16]);
    uuid::Builder::from_sha1_bytes(bytes).into_uuid()
}
