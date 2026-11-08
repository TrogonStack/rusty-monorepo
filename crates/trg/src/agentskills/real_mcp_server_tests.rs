use super::*;

#[test]
fn a_real_server_started_without_the_flag_is_refused_by_name() {
    let command = RealServerCommand::new("./fixture-server".to_string(), vec!["--stdio".to_string()]);

    let err = admit_real_server(&command, RealServerTrust::default()).unwrap_err();

    assert!(
        matches!(err, RealServerError::NotGranted { .. }),
        "refusal should be the named NotGranted variant: {err:?}"
    );
    let message = err.to_string();
    assert!(
        message.contains("--allow-real-mcp-server"),
        "refusal should name the opt-in flag: {message}"
    );
    assert!(
        message.contains("./fixture-server (1 arg)"),
        "refusal should name the command that was withheld: {message}"
    );
    assert!(
        !message.contains("--stdio"),
        "refusal should not leak the argument value: {message}"
    );
}

#[test]
fn displaying_a_command_never_prints_an_argument_value() {
    let command = RealServerCommand::new(
        "./mcp-servers/github".to_string(),
        vec!["--token".to_string(), "sk-super-secret".to_string()],
    );

    let rendered = command.to_string();

    assert_eq!(rendered, "./mcp-servers/github (2 args)");
    assert!(!rendered.contains("sk-super-secret"), "rendered: {rendered}");
    assert!(!rendered.contains("--token"), "rendered: {rendered}");
}

#[test]
fn a_real_server_started_with_the_flag_is_admitted() {
    let command = RealServerCommand::new("./fixture-server".to_string(), vec![]);

    assert!(admit_real_server(&command, RealServerTrust::granted(true)).is_ok());
}
