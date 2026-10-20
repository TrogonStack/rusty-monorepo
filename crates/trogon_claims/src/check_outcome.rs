use crate::constants::{
    LOOKUP_PERMISSIONSHIP_CONDITIONAL_PERMISSION, LOOKUP_PERMISSIONSHIP_HAS_PERMISSION,
    PERMISSIONSHIP_CONDITIONAL_PERMISSION, PERMISSIONSHIP_HAS_PERMISSION, PERMISSIONSHIP_NO_PERMISSION,
};
use crate::outcome_error::OutcomeError;
use crate::param_name::ParamName;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    HasPermission,
    NoPermission,
    Conditional { missing: Vec<ParamName> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupOutcome {
    HasPermission,
    Conditional { missing: Vec<ParamName> },
}

fn missing_parameters<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<Vec<ParamName>, OutcomeError> {
    names
        .into_iter()
        .map(|name| ParamName::try_new(name.to_owned()).map_err(OutcomeError::from))
        .collect()
}

fn classify_unrecognized(value: i32) -> OutcomeError {
    if value == 0 {
        OutcomeError::Unspecified
    } else {
        OutcomeError::Unknown { value }
    }
}

impl CheckOutcome {
    pub fn try_from_wire<'a>(
        permissionship: i32,
        missing_required_context: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, OutcomeError> {
        match permissionship {
            PERMISSIONSHIP_NO_PERMISSION => Ok(Self::NoPermission),
            PERMISSIONSHIP_HAS_PERMISSION => Ok(Self::HasPermission),
            PERMISSIONSHIP_CONDITIONAL_PERMISSION => Ok(Self::Conditional {
                missing: missing_parameters(missing_required_context)?,
            }),
            other => Err(classify_unrecognized(other)),
        }
    }
}

impl LookupOutcome {
    pub fn try_from_wire<'a>(
        permissionship: i32,
        missing_required_context: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, OutcomeError> {
        match permissionship {
            LOOKUP_PERMISSIONSHIP_HAS_PERMISSION => Ok(Self::HasPermission),
            LOOKUP_PERMISSIONSHIP_CONDITIONAL_PERMISSION => Ok(Self::Conditional {
                missing: missing_parameters(missing_required_context)?,
            }),
            other => Err(classify_unrecognized(other)),
        }
    }
}
