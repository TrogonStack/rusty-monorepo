use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use rustls::pki_types::CertificateDer;

const DEFAULT_DEADLINE: Duration = Duration::from_millis(200);
const DEFAULT_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const WASM_PAGE_BYTES: usize = 64 * 1024;
const MAX_MEMORY_BYTES: usize = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HookPolicy {
    FailOpen,
    #[default]
    FailClosed,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("hook policy {0:?} must be fail-open or fail-closed")]
pub struct HookPolicyError(String);

impl HookPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FailOpen => "fail-open",
            Self::FailClosed => "fail-closed",
        }
    }
}

impl FromStr for HookPolicy {
    type Err = HookPolicyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fail-open" | "fail_open" => Ok(Self::FailOpen),
            "fail-closed" | "fail_closed" => Ok(Self::FailClosed),
            other => Err(HookPolicyError(other.to_owned())),
        }
    }
}

impl fmt::Display for HookPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HookDeadline(Duration);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("hook deadline must be greater than zero")]
pub struct HookDeadlineError;

impl HookDeadline {
    pub fn get(self) -> Duration {
        self.0
    }
}

impl Default for HookDeadline {
    fn default() -> Self {
        Self(DEFAULT_DEADLINE)
    }
}

impl TryFrom<Duration> for HookDeadline {
    type Error = HookDeadlineError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        if value.is_zero() {
            Err(HookDeadlineError)
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HookMemoryLimit(usize);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HookMemoryLimitError {
    #[error("hook memory {0:?} is not a size like 64MiB, 512KiB or 1GiB")]
    Unparsable(String),
    #[error("hook memory must be between {WASM_PAGE_BYTES} bytes and {MAX_MEMORY_BYTES} bytes, got {0}")]
    OutOfRange(u64),
}

impl HookMemoryLimit {
    pub fn bytes(self) -> usize {
        self.0
    }
}

impl Default for HookMemoryLimit {
    fn default() -> Self {
        Self(DEFAULT_MEMORY_BYTES)
    }
}

impl TryFrom<u64> for HookMemoryLimit {
    type Error = HookMemoryLimitError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match usize::try_from(value) {
            Ok(bytes) if (WASM_PAGE_BYTES..=MAX_MEMORY_BYTES).contains(&bytes) => Ok(Self(bytes)),
            _ => Err(HookMemoryLimitError::OutOfRange(value)),
        }
    }
}

impl FromStr for HookMemoryLimit {
    type Err = HookMemoryLimitError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let unparsable = || HookMemoryLimitError::Unparsable(s.to_owned());
        let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let (digits, unit) = s.split_at(split);
        let value: u64 = digits.parse().map_err(|_| unparsable())?;
        let scale: u64 = match unit {
            "" | "B" => 1,
            "KiB" => 1 << 10,
            "MiB" => 1 << 20,
            "GiB" => 1 << 30,
            _ => return Err(unparsable()),
        };
        Self::try_from(value.checked_mul(scale).ok_or_else(unparsable)?)
    }
}

impl fmt::Display for HookMemoryLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}B", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HttpsPort(u16);

impl HttpsPort {
    pub const DEFAULT: Self = Self(443);

    pub fn get(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("https port must be between 1 and 65535")]
pub struct HttpsPortError;

impl TryFrom<u16> for HttpsPort {
    type Error = HttpsPortError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Err(HttpsPortError),
            port => Ok(Self(port)),
        }
    }
}

impl fmt::Display for HttpsPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AllowedHost {
    host: String,
    port: HttpsPort,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("allowed host {0:?} must be a DNS name or IP address, optionally followed by :port, of A-Z, a-z, 0-9, '.', '-' and ':'")]
pub struct AllowedHostError(String);

impl AllowedHost {
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> HttpsPort {
        self.port
    }

    pub fn ip(&self) -> Option<IpAddr> {
        self.host.parse().ok()
    }

    pub fn matches(&self, host: &str) -> bool {
        self.host
            .eq_ignore_ascii_case(host.trim_start_matches('[').trim_end_matches(']'))
    }

    pub fn matches_authority(&self, host: &str, port: HttpsPort) -> bool {
        self.port == port && self.matches(host)
    }

    fn split(s: &str) -> Option<(&str, Option<&str>)> {
        if let Some(rest) = s.strip_prefix('[') {
            let (host, tail) = rest.split_once(']')?;
            return match tail {
                "" => Some((host, None)),
                _ => Some((host, Some(tail.strip_prefix(':')?))),
            };
        }
        match s.matches(':').count() {
            0 => Some((s, None)),
            1 => s.split_once(':').map(|(host, port)| (host, Some(port))),
            _ => Some((s, None)),
        }
    }
}

impl FromStr for AllowedHost {
    type Err = AllowedHostError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || AllowedHostError(s.to_owned());
        let (host, port) = Self::split(s).ok_or_else(invalid)?;
        let valid_host = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b':');
        if !valid_host {
            return Err(invalid());
        }
        let port = match port {
            None => HttpsPort::DEFAULT,
            Some(port) => port
                .parse::<u16>()
                .ok()
                .and_then(|port| HttpsPort::try_from(port).ok())
                .ok_or_else(invalid)?,
        };
        Ok(Self {
            host: host.to_ascii_lowercase(),
            port,
        })
    }
}

impl fmt::Display for AllowedHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TrustAnchors {
    #[default]
    WebPkiRoots,
    Pinned(Vec<CertificateDer<'static>>),
}

impl TrustAnchors {
    pub fn pinned(certificates: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self::Pinned(certificates.into_iter().map(CertificateDer::from).collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookConfig {
    pub component_path: PathBuf,
    pub policy: HookPolicy,
    pub deadline: HookDeadline,
    pub memory_limit: HookMemoryLimit,
    pub http_allow: Vec<AllowedHost>,
    pub https_trust: TrustAnchors,
}

impl HookConfig {
    pub fn new(component_path: impl Into<PathBuf>) -> Self {
        Self {
            component_path: component_path.into(),
            policy: HookPolicy::default(),
            deadline: HookDeadline::default(),
            memory_limit: HookMemoryLimit::default(),
            http_allow: Vec::new(),
            https_trust: TrustAnchors::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_policies() {
        assert_eq!("fail-open".parse(), Ok(HookPolicy::FailOpen));
        assert_eq!("fail_closed".parse(), Ok(HookPolicy::FailClosed));
        assert!("open".parse::<HookPolicy>().is_err());
    }

    #[test]
    fn parses_memory_sizes() -> Result<(), HookMemoryLimitError> {
        assert_eq!("64MiB".parse::<HookMemoryLimit>()?.bytes(), 64 << 20);
        assert_eq!("512KiB".parse::<HookMemoryLimit>()?.bytes(), 512 << 10);
        assert_eq!("1GiB".parse::<HookMemoryLimit>()?.bytes(), 1 << 30);
        assert_eq!("65536".parse::<HookMemoryLimit>()?.bytes(), 65536);
        assert!("64MB".parse::<HookMemoryLimit>().is_err());
        assert!("1KiB".parse::<HookMemoryLimit>().is_err());
        assert!("5GiB".parse::<HookMemoryLimit>().is_err());
        Ok(())
    }

    #[test]
    fn matches_hosts_case_insensitively() -> Result<(), AllowedHostError> {
        let host: AllowedHost = "API.Example.com".parse()?;
        assert!(host.matches("api.example.COM"));
        assert!(!host.matches("evil.example.com"));
        assert!("::1".parse::<AllowedHost>()?.matches("[::1]"));
        assert!("a/b".parse::<AllowedHost>().is_err());
        Ok(())
    }

    #[test]
    fn parses_authorities_with_ports() -> Result<(), AllowedHostError> {
        let plain: AllowedHost = "api.example.com".parse()?;
        assert_eq!(plain.port(), HttpsPort::DEFAULT);
        let ported: AllowedHost = "127.0.0.1:8443".parse()?;
        assert_eq!((ported.host(), ported.port().get()), ("127.0.0.1", 8443));
        assert!(ported.ip().is_some());
        let v6: AllowedHost = "[::1]:9443".parse()?;
        assert_eq!((v6.host(), v6.port().get()), ("::1", 9443));
        assert_eq!(v6.to_string(), "[::1]:9443");
        assert!("api.example.com:0".parse::<AllowedHost>().is_err());
        assert!("api.example.com:https".parse::<AllowedHost>().is_err());
        Ok(())
    }

    #[test]
    fn rejects_zero_deadline() {
        assert!(HookDeadline::try_from(Duration::ZERO).is_err());
    }
}
