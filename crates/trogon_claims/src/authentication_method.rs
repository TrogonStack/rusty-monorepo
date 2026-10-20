use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AuthenticationMethod {
    Face,
    Fpt,
    Geo,
    Hwk,
    Iris,
    Kba,
    Mca,
    Mfa,
    Otp,
    Pin,
    Pwd,
    Rba,
    Retina,
    Sc,
    Sms,
    Swk,
    Tel,
    User,
    Vbm,
    Wia,
    Other(Cow<'static, str>),
}

impl AuthenticationMethod {
    pub fn from_value(value: impl Into<Cow<'static, str>>) -> Self {
        let value = value.into();
        match value.as_ref() {
            "face" => Self::Face,
            "fpt" => Self::Fpt,
            "geo" => Self::Geo,
            "hwk" => Self::Hwk,
            "iris" => Self::Iris,
            "kba" => Self::Kba,
            "mca" => Self::Mca,
            "mfa" => Self::Mfa,
            "otp" => Self::Otp,
            "pin" => Self::Pin,
            "pwd" => Self::Pwd,
            "rba" => Self::Rba,
            "retina" => Self::Retina,
            "sc" => Self::Sc,
            "sms" => Self::Sms,
            "swk" => Self::Swk,
            "tel" => Self::Tel,
            "user" => Self::User,
            "vbm" => Self::Vbm,
            "wia" => Self::Wia,
            _ => Self::Other(value),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Face => "face",
            Self::Fpt => "fpt",
            Self::Geo => "geo",
            Self::Hwk => "hwk",
            Self::Iris => "iris",
            Self::Kba => "kba",
            Self::Mca => "mca",
            Self::Mfa => "mfa",
            Self::Otp => "otp",
            Self::Pin => "pin",
            Self::Pwd => "pwd",
            Self::Rba => "rba",
            Self::Retina => "retina",
            Self::Sc => "sc",
            Self::Sms => "sms",
            Self::Swk => "swk",
            Self::Tel => "tel",
            Self::User => "user",
            Self::Vbm => "vbm",
            Self::Wia => "wia",
            Self::Other(value) => value,
        }
    }
}
