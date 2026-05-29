use std::collections::BTreeMap;

use regex::Regex;
use std::sync::LazyLock;

const REDACTED: &str = "<redacted>";

static BEARER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._\-]{16,}").unwrap());
static AWS_ACCESS_KEY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"AKIA[0-9A-Z]{16}").unwrap());
static GITHUB_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"ghp_[A-Za-z0-9]{36}").unwrap());
static JWT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}").unwrap());

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedTranscript(pub String);

impl RedactedTranscript {
    pub fn into_inner(self) -> String {
        self.0
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
mod tests {
    use super::*;

    #[test]
    fn redact_secrets_scrubs_bearer_token() {
        let input = "Authorization: Bearer abcdefghijklmnop";
        assert_eq!(redact_secrets(input), "Authorization: <redacted>");
    }

    #[test]
    fn redact_secrets_scrubs_aws_and_github_tokens() {
        let github = "ghp_123456789012345678901234567890123456";
        let input = format!("keys AKIAIOSFODNN7EXAMPLE and {github}");
        let out = redact_secrets(&input);
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!out.contains(github));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redact_secrets_scrubs_jwt_shaped_strings() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let out = redact_secrets(&format!("token={jwt}"));
        assert!(!out.contains(jwt));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redact_secrets_leaves_benign_text() {
        let input = "The report includes a summary of sales data.";
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn redact_env_strips_secret_keys() {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert("OPENAI_API_KEY".to_string(), "secret".to_string());
        env.insert("GITHUB_TOKEN".to_string(), "ghp_x".to_string());
        env.insert("AWS_SECRET_ACCESS_KEY".to_string(), "aws".to_string());
        env.insert("MY_PASSWORD".to_string(), "pw".to_string());
        env.insert("SAFE".to_string(), "ok".to_string());

        let filtered: BTreeMap<_, _> = env.into_iter().filter(|(key, _)| !is_secret_env_key(key)).collect();

        assert!(filtered.contains_key("PATH"));
        assert!(filtered.contains_key("SAFE"));
        assert!(!filtered.contains_key("OPENAI_API_KEY"));
        assert!(!filtered.contains_key("GITHUB_TOKEN"));
        assert!(!filtered.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!filtered.contains_key("MY_PASSWORD"));
    }

    #[test]
    fn redact_command_args_redacts_sensitive_flags_and_secret_values() {
        let line = redact_command_args(
            "codex",
            &[
                "exec",
                "--api-key",
                "sk-secretvalue1234567890",
                "--model",
                "gpt-4",
                "prompt",
            ],
        );
        assert!(line.0.contains("--api-key"));
        assert!(!line.0.contains("sk-secretvalue1234567890"));
        assert!(line.0.contains("gpt-4"));
    }

    #[test]
    fn redact_command_args_redacts_inline_flag_values() {
        let line = redact_command_args("tool", &["run", "--token=ghp_123456789012345678901234567890123456"]);
        assert!(!line.0.contains("ghp_"));
        assert!(line.0.contains("--token=<redacted>"));
    }
}
