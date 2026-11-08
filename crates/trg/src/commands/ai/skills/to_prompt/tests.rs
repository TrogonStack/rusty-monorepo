/// A prompt that is missing a skill the caller asked for.
///
/// The command still prints the skills it could read, so the only thing
/// separating a complete prompt from a short one is the exit code. It has to
/// say so under either format, since `--output-format text` is what a shell
/// script redirecting stdout to a file uses.
#[cfg(test)]
mod partial_reads_tests;
