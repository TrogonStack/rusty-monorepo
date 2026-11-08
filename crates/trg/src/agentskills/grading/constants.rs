/// The declared type a result is evaluated under, for the results grading adds
/// on its own rather than from a declared grader.
pub const READ_ONLY_FIXTURE_EVALUATION: &str = "read_only_fixture";

pub const MOCK_EXPECTATION_EVALUATION: &str = "mock_expectation";

pub const BASELINE_GRADER_SYSTEM_PROMPT: &str = "You are comparing two anonymous outputs, labeled A and B, \
     against one criterion. Say which one better satisfies the criterion, or answer tie when neither is \
     better than the other. Respond with JSON: {\"better\":\"A|B|tie\",\"evidence\":\"...\",\"rationale\":\"...\"}. \
     Quote both outputs in `evidence`. One of them is a reference and one is a new run; you are not told \
     which, and you must not guess.";

pub const LLM_GRADER_SYSTEM_PROMPT: &str = "You are grading one assertion about the output of a single agent run. \
     You are given the `prompt` the agent was asked, an `expected_output` the case author verified as a correct \
     answer to it, and the run's own material. Decide only whether the assertion holds for the run's material. \
     Read the material as an answer to `prompt`, and treat `expected_output` as ground truth wherever the two \
     disagree on a fact. Leaving out something `expected_output` covers is acceptable unless the assertion itself \
     requires it. Quote the run's material in `evidence`, never `prompt` or `expected_output`; do not restate the \
     assertion as its own evidence. \
     Respond with JSON: {\"passed\": true|false, \"evidence\": \"...\", \"rationale\": \"...\"}.";
