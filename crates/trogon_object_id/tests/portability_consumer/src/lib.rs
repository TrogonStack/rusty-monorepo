#![no_std]

use core::fmt::{self, Write};
use ids::{Definition, Domain, ObjectId, ParseError, Payload};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Digit(bool);
impl fmt::Display for Digit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(if self.0 { "7" } else { "invalid" })
    }
}
impl Payload for Digit {
    type Raw = Self;
    fn parse(value: &str) -> Result<Self, ParseError> {
        if value == "7" { Ok(Self(true)) } else { Err(ParseError::InvalidValue) }
    }
    fn validate(&self) -> Result<(), ParseError> {
        if self.0 { Ok(()) } else { Err(ParseError::InvalidValue) }
    }
    fn as_raw(&self) -> &Self { self }
}
struct Item;
impl Domain for Item {
    const DEFINITION: Definition = Definition::new("item", "_");
    type Payload = Digit;
}

pub struct Buffer {
    bytes: [u8; 64],
    len: usize,
}
impl Write for Buffer {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.len.checked_add(value.len()).ok_or(fmt::Error)?;
        self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?.copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
}

pub fn exercise() -> Result<(), ParseError> {
    let id = ObjectId::<Item>::try_from("item_7")?;
    let mut output = Buffer { bytes: [0; 64], len: 0 };
    assert!(write!(&mut output, "{id}").is_ok());
    assert_eq!(&output.bytes[..output.len], b"item_7");
    assert!(ObjectId::<Item>::try_from("wrong_7").is_err());
    assert!(ObjectId::<Item>::try_from_payload(Digit(false)).is_err());
    let mut short = Buffer { bytes: [0; 64], len: 63 };
    assert!(write!(&mut short, "{id}").is_err());
    #[cfg(feature = "derive")]
    derived()?;
    #[cfg(feature = "encoding")]
    {
        use serde::Deserialize;
        let input = serde::de::value::StrDeserializer::<serde::de::value::Error>::new("item_7");
        assert!(ObjectId::<Item>::deserialize(input).is_ok());
        let raw = serde::de::value::StrDeserializer::<serde::de::value::Error>::new("7");
        let decoded: Result<ObjectId<Item>, _> = ids::serde_raw::deserialize(raw);
        assert!(decoded.is_ok());
    }
    Ok(())
}

#[cfg(feature = "derive")]
fn derived() -> Result<(), ParseError> {
    #[derive(Debug, ids::ObjectId)]
    #[object_id(prefix = "direct", fallible)]
    #[repr(transparent)]
    struct Direct(Digit);
    #[derive(ids::ObjectId)]
    enum Either { Direct(Direct) }
    let direct = Direct::try_from("direct_7")?;
    let _ = Either::from(direct);
    assert!(Either::try_from("direct_7").is_ok());
    #[cfg(feature = "encoding")]
    {
        use serde::Deserialize;
        let input = serde::de::value::StrDeserializer::<serde::de::value::Error>::new("direct_7");
        assert!(Either::deserialize(input).is_ok());
    }
    #[cfg(feature = "binary")]
    {
        #[derive(ids::ObjectId)]
        #[object_id(prefix = "uuid")]
        #[repr(transparent)]
        struct UuidId(ids::Uuid);
        let id = UuidId::try_from("uuid_550e8400-e29b-41d4-a716-446655440000")?;
        assert_eq!(core::mem::size_of_val(&id), 16);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn public_core_operations() { super::exercise().expect("public compact operations"); }
}
