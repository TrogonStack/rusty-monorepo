use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use async_nats::jetstream::message::StreamMessage;
use async_nats::jetstream::stream::{LastRawMessageError, LastRawMessageErrorKind, Stream};

use crate::constants::{
    DEFAULT_READ_REQUEST_TIMEOUT, JETSTREAM_REQUEST_TIMEOUT, MAX_READ_REQUEST_TIMEOUT, MIN_READ_REQUEST_TIMEOUT,
};

/// How long one leader read waits for an answer before it is sent again.
///
/// A read sent while the stream has no leader is dropped rather than refused, so it waits out its whole
/// timeout. Sending again on a shorter timeout lets a read land soon after a new leader is elected, while the
/// attempts together stay within the JetStream request timeout a single read used to wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReadRequestTimeout(Duration);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadRequestTimeoutError {
    #[error(
        "read request timeout must be between {MIN_READ_REQUEST_TIMEOUT:?} and {MAX_READ_REQUEST_TIMEOUT:?}, got {0:?}"
    )]
    OutOfRange(Duration),
    #[error("read request timeout must be a whole number of ms or s, like 500ms or 1s, got {0:?}")]
    Unparsable(String),
}

#[derive(Debug, thiserror::Error)]
#[error("leader read timed out after {attempts} attempts of {timeout:?}")]
struct ReadTimedOut {
    attempts: u32,
    timeout: Duration,
}

impl ReadRequestTimeout {
    pub fn get(self) -> Duration {
        self.0
    }

    /// How many times a read is sent before it gives up: as many as fit in the JetStream request timeout.
    pub fn attempts(self) -> u32 {
        let budget = JETSTREAM_REQUEST_TIMEOUT.as_millis();
        let each = self.0.as_millis().max(1);
        u32::try_from(budget.div_ceil(each)).unwrap_or(u32::MAX).max(1)
    }

    /// Reads the last message stored on `subject`, sending the request again each time it goes unanswered.
    pub async fn last_message(self, stream: &Stream, subject: &str) -> Result<StreamMessage, LastRawMessageError> {
        let attempts = self.attempts();
        for _ in 0..attempts {
            if let Ok(read) = tokio::time::timeout(self.0, stream.get_last_raw_message_by_subject(subject)).await {
                return read;
            }
        }
        Err(LastRawMessageError::with_source(
            LastRawMessageErrorKind::Other,
            ReadTimedOut {
                attempts,
                timeout: self.0,
            },
        ))
    }
}

impl Default for ReadRequestTimeout {
    fn default() -> Self {
        Self(DEFAULT_READ_REQUEST_TIMEOUT)
    }
}

impl TryFrom<Duration> for ReadRequestTimeout {
    type Error = ReadRequestTimeoutError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        if (MIN_READ_REQUEST_TIMEOUT..=MAX_READ_REQUEST_TIMEOUT).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ReadRequestTimeoutError::OutOfRange(value))
        }
    }
}

impl FromStr for ReadRequestTimeout {
    type Err = ReadRequestTimeoutError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let unparsable = || ReadRequestTimeoutError::Unparsable(raw.to_owned());
        let trimmed = raw.trim();
        let (digits, scale) = if let Some(millis) = trimmed.strip_suffix("ms") {
            (millis, Duration::from_millis(1))
        } else if let Some(seconds) = trimmed.strip_suffix('s') {
            (seconds, Duration::from_secs(1))
        } else {
            return Err(unparsable());
        };
        let count = digits.parse::<u32>().map_err(|_| unparsable())?;
        Self::try_from(scale * count)
    }
}

impl fmt::Display for ReadRequestTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.0.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parses_milliseconds_and_seconds_within_range() -> TestResult {
        assert_eq!("500ms".parse::<ReadRequestTimeout>()?.get(), Duration::from_millis(500));
        assert_eq!(" 2s ".parse::<ReadRequestTimeout>()?.get(), Duration::from_secs(2));
        for bad in ["", "1", "ms", "1.5s", "-1s", "1m", "50ms", "6s"] {
            assert!(bad.parse::<ReadRequestTimeout>().is_err(), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn display_round_trips() -> TestResult {
        let timeout = ReadRequestTimeout::default();
        assert_eq!(timeout.to_string().parse::<ReadRequestTimeout>()?, timeout);
        Ok(())
    }

    #[test]
    fn attempts_fit_within_the_jetstream_request_timeout() -> TestResult {
        assert_eq!(ReadRequestTimeout::try_from(MAX_READ_REQUEST_TIMEOUT)?.attempts(), 1);
        assert_eq!(ReadRequestTimeout::try_from(Duration::from_secs(1))?.attempts(), 5);
        assert_eq!(ReadRequestTimeout::try_from(Duration::from_millis(1500))?.attempts(), 4);
        let shortest = ReadRequestTimeout::try_from(MIN_READ_REQUEST_TIMEOUT)?;
        assert!(shortest.get() * shortest.attempts() >= JETSTREAM_REQUEST_TIMEOUT);
        Ok(())
    }
}
