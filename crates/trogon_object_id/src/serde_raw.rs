use crate::Identifier;
/// Encodes only the raw payload, so storage must establish the identifier's domain.
pub fn serialize<T: Identifier, S: serde::Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value.as_raw())
}
/// Decodes an unprefixed string through the identifier's checked raw constructor.
///
/// The adapter accepts owned strings without discarding their allocation.
pub fn deserialize<'de, T: Identifier, D: serde::Deserializer<'de>>(deserializer: D) -> Result<T, D::Error> {
    crate::__private::deserialize_owned(deserializer, T::try_from_raw, T::try_from_owned)
}
