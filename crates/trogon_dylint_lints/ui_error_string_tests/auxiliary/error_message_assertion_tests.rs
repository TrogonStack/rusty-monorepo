use super::io;

fn assert_redacted_message(error: io::Error) {
    let message = error.to_string();
    assert!(message.contains("unavailable"));
    assert!(!message.contains("credential-value"));
}

#[test]
fn error_messages_are_part_of_the_diagnostic_contract() {
    let error = io::Error::other("service unavailable");
    assert_eq!(error.to_string(), "service unavailable");
    assert_redacted_message(error);
}
