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
    let jwt =
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
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
