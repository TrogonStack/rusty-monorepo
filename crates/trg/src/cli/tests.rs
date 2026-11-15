#[cfg(test)]
mod command_identity_tests;

/// Every `trg` invocation printed in the docs, handed to the real parser.
///
/// A flag that was renamed leaves the docs telling people to run something
/// clap now rejects, and nothing catches it: the example is prose to the
/// compiler and a command to the reader. Parsing is enough to find that,
/// and it runs nothing.
#[cfg(test)]
mod doc_commands_tests;

/// One spelling of the output choice, everywhere it is offered.
///
/// Two commands that both render a result and disagree about how to ask for
/// the machine-readable one leave a caller reading `--help` for each of them.
/// Sharing [`OutputFormat`] gets the type right; only walking the built command
/// tree gets the flag right, since a command is free to declare its own.
#[cfg(test)]
mod output_format_tests;
