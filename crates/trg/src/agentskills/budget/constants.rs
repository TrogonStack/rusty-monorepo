/// The failure kind recorded on a run the ledger refused to start.
pub const FAILURE_KIND_BUDGET: &str = "budget";

pub const MICRO_USD_PER_USD: f64 = 1_000_000.0;

/// The failure kind recorded on a run stopped for making more tool calls than its cap.
pub const FAILURE_KIND_TOOL_CALL_CAP: &str = "tool_call_cap";

/// The failure kind recorded on a run stopped for taking more turns than its cap.
pub const FAILURE_KIND_TURN_CAP: &str = "turn_cap";
