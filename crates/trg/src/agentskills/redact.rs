mod constants;
use constants::REDACTED;
use std::collections::BTreeMap;

use regex::Regex;
use std::sync::LazyLock;

static BEARER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._\-]{16,}").expect("the bearer token pattern is a valid regex")
});
static AWS_ACCESS_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"AKIA[0-9A-Z]{16}").expect("the AWS access key pattern is a valid regex"));
static GITHUB_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ghp_[A-Za-z0-9]{36}").expect("the GitHub token pattern is a valid regex"));
static JWT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}")
        .expect("the JWT pattern is a valid regex")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedTranscript(pub String);

impl RedactedTranscript {
    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedCommandLine(pub String);

impl RedactedCommandLine {
    pub fn into_inner(self) -> String {
        self.0
    }
}

pub fn redact_secrets(text: &str) -> String {
    let mut out = text.to_string();
    for pattern in [&*BEARER_RE, &*AWS_ACCESS_KEY_RE, &*GITHUB_TOKEN_RE, &*JWT_RE] {
        out = pattern.replace_all(&out, REDACTED).into_owned();
    }
    out
}

pub fn redact_transcript_bytes(raw: &[u8]) -> RedactedTranscript {
    let text = String::from_utf8_lossy(raw);
    RedactedTranscript(redact_secrets(&text))
}

pub fn is_secret_env_key(key: &str) -> bool {
    let upper = key.to_uppercase();
    upper.ends_with("_TOKEN")
        || upper.ends_with("_SECRET")
        || upper.ends_with("_KEY")
        || upper.ends_with("_PASSWORD")
        || upper.starts_with("AWS_")
}

pub fn redact_env() -> BTreeMap<String, String> {
    std::env::vars().filter(|(key, _)| !is_secret_env_key(key)).collect()
}

fn is_sensitive_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--api-key"
            | "--api_key"
            | "--token"
            | "--secret"
            | "--password"
            | "--access-token"
            | "--access_token"
            | "--auth-token"
            | "--auth_token"
            | "-k"
            | "--key"
    )
}

fn looks_like_secret_value(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    redact_secrets(value) != value
}

pub fn redact_command_args(program: &str, args: &[&str]) -> RedactedCommandLine {
    let mut redacted = Vec::with_capacity(args.len() + 1);
    redacted.push(program.to_string());

    let mut i = 0;
    while i < args.len() {
        let arg = args[i];
        if arg.starts_with("--") && arg.contains('=') {
            let (flag, value) = arg.split_once('=').unwrap_or((arg, ""));
            if is_sensitive_flag(flag) || looks_like_secret_value(value) {
                redacted.push(format!("{flag}={REDACTED}"));
            } else {
                redacted.push(arg.to_string());
            }
            i += 1;
            continue;
        }

        if is_sensitive_flag(arg) {
            redacted.push(arg.to_string());
            if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                redacted.push(REDACTED.to_string());
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        if looks_like_secret_value(arg) {
            redacted.push(REDACTED.to_string());
        } else {
            redacted.push(arg.to_string());
        }
        i += 1;
    }

    RedactedCommandLine(redacted.join(" "))
}

#[cfg(test)]
#[path = "redact_tests.rs"]
mod tests;
