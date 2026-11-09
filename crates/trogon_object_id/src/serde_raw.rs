use crate::Identifier;
/// Encodes only the raw payload, so storage must establish the identifier's domain.
pub fn serialize<T: Identifier, S: serde::Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value.as_raw())
}
/// Decodes an unprefixed string through the identifier's checked raw constructor.
///
/// With `alloc`, the adapter accepts owned strings and reuses their storage.
pub fn deserialize<'de, T: Identifier, D: serde::Deserializer<'de>>(deserializer: D) -> Result<T, D::Error> {
    crate::__deserialize!(deserializer, T::try_from_raw, T::try_from_owned)
}
