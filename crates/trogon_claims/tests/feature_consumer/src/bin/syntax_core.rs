use trogon_claims_syntax::{validate_client_id, validate_principal_kind, validate_subject_id, SyntaxRule};

fn core_rule(rule: SyntaxRule) -> &'static str {
    match rule {
        SyntaxRule::NonEmpty => "nonempty",
        SyntaxRule::PrintableAscii => "printable ASCII",
    }
}

fn main() {
    assert!(validate_subject_id("user@example.com:42 é").is_ok());
    assert!(validate_principal_kind("人/Person").is_ok());
    assert!(validate_client_id("client id!~").is_ok());
    assert_eq!(core_rule(validate_subject_id("").unwrap_err().rule()), "nonempty");
}
