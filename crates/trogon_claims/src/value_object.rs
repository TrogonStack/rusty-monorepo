macro_rules! string_value_object {
    ($(#[$meta:meta])* $name:ident, $kind:expr, $validate:path) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(::std::borrow::Cow<'static, str>);

        impl $name {
            pub fn try_new(
                value: impl Into<::std::borrow::Cow<'static, str>>,
            ) -> Result<Self, $crate::errors::ValueObjectError> {
                let value = value.into();
                $validate(&value)
                    .map_err(|violation| $crate::errors::ValueObjectError::new($kind, violation))?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0.into_owned()
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::errors::ValueObjectError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_new(value.to_owned())
            }
        }

        impl TryFrom<&str> for $name {
            type Error = $crate::errors::ValueObjectError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_new(value.to_owned())
            }
        }

        impl TryFrom<String> for $name {
            type Error = $crate::errors::ValueObjectError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }
    };
}

pub(crate) use string_value_object;
