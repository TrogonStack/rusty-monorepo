use std::time::Duration;

use axum_core::response::{IntoResponse, Response};
use http::header::WWW_AUTHENTICATE;
use http::{HeaderValue, StatusCode};
use thiserror::Error;

use crate::authentication_context::AuthenticationContext;
use crate::claim_text::ClaimText;
use crate::constants::{CHALLENGE_STEP_UP_ERROR, STEP_UP_ERROR_DESCRIPTION};
use crate::numeric_date::NumericDate;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum StepUpRequirementError {
    #[error("a step-up requirement needs at least one acr value or a max age")]
    Empty,
    #[error("an acr value must be a non-empty run of visible ASCII without quotes, backslashes or spaces")]
    InvalidAcrValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepUpRequirement {
    acr_values: Vec<ClaimText>,
    max_age: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepUpRequired {
    requirement: StepUpRequirement,
}

fn is_acr_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
}

impl StepUpRequirement {
    pub fn try_new(acr_values: Vec<ClaimText>, max_age: Option<Duration>) -> Result<Self, StepUpRequirementError> {
        if acr_values.is_empty() && max_age.is_none() {
            return Err(StepUpRequirementError::Empty);
        }
        if !acr_values.iter().all(|value| is_acr_value(value.as_str())) {
            return Err(StepUpRequirementError::InvalidAcrValue);
        }
        Ok(Self { acr_values, max_age })
    }

    pub fn acr_values(&self) -> &[ClaimText] {
        &self.acr_values
    }

    pub fn max_age(&self) -> Option<Duration> {
        self.max_age
    }

    pub fn check(&self, context: &AuthenticationContext, now: NumericDate) -> Result<(), StepUpRequired> {
        if !self.acr_satisfied(context) || !self.age_satisfied(context, now) {
            return Err(StepUpRequired {
                requirement: self.clone(),
            });
        }
        Ok(())
    }

    fn acr_satisfied(&self, context: &AuthenticationContext) -> bool {
        self.acr_values.is_empty()
            || context
                .acr()
                .is_some_and(|acr| self.acr_values.iter().any(|accepted| accepted == acr))
    }

    fn age_satisfied(&self, context: &AuthenticationContext, now: NumericDate) -> bool {
        let Some(max_age) = self.max_age else {
            return true;
        };
        let Some(auth_time) = context.auth_time() else {
            return false;
        };
        let limit = i64::try_from(max_age.as_secs()).unwrap_or(i64::MAX);
        now.secs().saturating_sub(auth_time.secs()) <= limit
    }
}

impl StepUpRequired {
    pub fn requirement(&self) -> &StepUpRequirement {
        &self.requirement
    }

    fn challenge(&self) -> String {
        let mut challenge = format!("{CHALLENGE_STEP_UP_ERROR}, error_description=\"{STEP_UP_ERROR_DESCRIPTION}\"");
        if !self.requirement.acr_values.is_empty() {
            let values: Vec<&str> = self.requirement.acr_values.iter().map(ClaimText::as_str).collect();
            challenge.push_str(&format!(", acr_values=\"{}\"", values.join(" ")));
        }
        if let Some(max_age) = self.requirement.max_age {
            challenge.push_str(&format!(", max_age=\"{}\"", max_age.as_secs()));
        }
        challenge
    }
}

impl IntoResponse for StepUpRequired {
    fn into_response(self) -> Response {
        let value = HeaderValue::from_str(&self.challenge())
            .unwrap_or_else(|_| HeaderValue::from_static(CHALLENGE_STEP_UP_ERROR));
        (StatusCode::UNAUTHORIZED, [(WWW_AUTHENTICATE, value)], ()).into_response()
    }
}
