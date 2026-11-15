#[cfg(feature = "alloc")]
use alloc::string::String;
use core::fmt;
use core::marker::PhantomData;

pub fn serialize<T: fmt::Display, S: serde::Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

#[cfg(feature = "alloc")]
pub fn deserialize_owned<'de, T, E, D, F, O>(deserializer: D, parse: F, parse_owned: O) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    F: Fn(&str) -> Result<T, E>,
    O: Fn(String) -> Result<T, E>,
    E: fmt::Display,
{
    struct StringVisitor<T, F, O> {
        parse: F,
        parse_owned: O,
        marker: PhantomData<fn() -> T>,
    }
    impl<'de, T, E, F, O> serde::de::Visitor<'de> for StringVisitor<T, F, O>
    where
        F: Fn(&str) -> Result<T, E>,
        O: Fn(String) -> Result<T, E>,
        E: fmt::Display,
    {
        type Value = T;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a validated identifier string")
        }
        fn visit_str<E2: serde::de::Error>(self, value: &str) -> Result<T, E2> {
            (self.parse)(value).map_err(E2::custom)
        }
        fn visit_string<E2: serde::de::Error>(self, value: String) -> Result<T, E2> {
            (self.parse_owned)(value).map_err(E2::custom)
        }
    }
    deserializer.deserialize_str(StringVisitor {
        parse,
        parse_owned,
        marker: PhantomData,
    })
}

#[cfg(not(feature = "alloc"))]
pub fn deserialize<'de, T, E, D, F>(deserializer: D, parse: F) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    F: Fn(&str) -> Result<T, E>,
    E: fmt::Display,
{
    struct StringVisitor<T, F> {
        parse: F,
        marker: PhantomData<fn() -> T>,
    }
    impl<'de, T, E, F> serde::de::Visitor<'de> for StringVisitor<T, F>
    where
        F: Fn(&str) -> Result<T, E>,
        E: fmt::Display,
    {
        type Value = T;
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a validated identifier string")
        }
        fn visit_str<E2: serde::de::Error>(self, value: &str) -> Result<T, E2> {
            (self.parse)(value).map_err(E2::custom)
        }
    }
    deserializer.deserialize_str(StringVisitor {
        parse,
        marker: PhantomData,
    })
}
