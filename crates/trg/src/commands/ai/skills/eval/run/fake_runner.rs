mod constants;
use constants::LANE_RENDEZVOUS;
use std::io::Write;

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::agentskills::budget::RunCost;
use crate::agentskills::runner::usage::HarnessTokenUsage;
use crate::agentskills::runner::{EvalRunOutcome, EvalRunRequest, RunStatus};

/// The fake runner's state for one test.
///
/// Shared behind an `Arc` rather than held in thread-local cells alone, because a run
/// executed in a lane is executed on another thread and still has to count against
/// the same test.
#[derive(Default)]
pub struct FakeState {
    enabled: AtomicBool,
    invocations: AtomicUsize,
    last_timeout_secs: Mutex<Option<u64>>,
    lanes: Mutex<LaneCensus>,
    joined: Condvar,
    tamper_case: Mutex<Option<String>>,
    remove_case: Mutex<Option<String>>,
    cost_usd: Mutex<Option<f64>>,
    transient_failures: AtomicUsize,
    log_mock_calls: AtomicBool,
}

#[derive(Default)]
struct LaneCensus {
    in_flight: usize,
    peak: usize,
    expected: usize,
}

thread_local! {
    static STATE: RefCell<Option<Arc<FakeState>>> = const { RefCell::new(None) };
}

fn state() -> Arc<FakeState> {
    STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        cell.get_or_insert_with(|| Arc::new(FakeState::default())).clone()
    })
}

pub fn enable() {
    state().enabled.store(true, Ordering::SeqCst);
}

pub fn disable() {
    state().enabled.store(false, Ordering::SeqCst);
}

pub fn reset() {
    let state = state();
    state.invocations.store(0, Ordering::SeqCst);
    *state.last_timeout_secs.lock().expect("fake timeout") = None;
    *state.lanes.lock().expect("fake lanes") = LaneCensus::default();
    *state.tamper_case.lock().expect("fake tamper") = None;
    *state.remove_case.lock().expect("fake remove") = None;
    *state.cost_usd.lock().expect("fake cost") = None;
    state.transient_failures.store(0, Ordering::SeqCst);
    state.log_mock_calls.store(false, Ordering::SeqCst);
}

/// Have this case's run rewrite the skill directory it was handed, standing in for an
/// agent that followed a staged path back out of its workspace.
pub fn tamper_on_case(case_id: &str) {
    *state().tamper_case.lock().expect("fake tamper") = Some(case_id.to_string());
}

/// Have this case's run delete the skill directory it was handed, standing in for an
/// agent that removed the directory the pass is meant to answer for.
pub fn remove_skill_on_case(case_id: &str) {
    *state().remove_case.lock().expect("fake remove") = Some(case_id.to_string());
}

/// Have every run from here on report this dollar cost, standing in for a runner that
/// prices what it did.
pub fn set_cost_usd(cost_usd: f64) {
    *state().cost_usd.lock().expect("fake cost") = Some(cost_usd);
}

/// Have the next runs fail the way a flaky harness does, so a test can tell what a
/// retried run cost in total from what the attempt that finally stuck cost.
pub fn fail_transiently_times(times: usize) {
    state().transient_failures.store(times, Ordering::SeqCst);
}

/// Record one mock call per invocation, in breach of what the case expected on the
/// attempts that are about to be discarded and within it on the one that sticks.
pub fn log_mock_calls() {
    state().log_mock_calls.store(true, Ordering::SeqCst);
}

/// Hold every run until this many are in flight, so a test can tell lanes that
/// overlapped from runs that merely happened to be fast.
pub fn expect_lanes(lanes: usize) {
    state().lanes.lock().expect("fake lanes").expected = lanes;
}

/// The most runs that were ever in flight at once.
pub fn peak_lanes() -> usize {
    state().lanes.lock().expect("fake lanes").peak
}

pub fn last_timeout_secs() -> Option<u64> {
    *state().last_timeout_secs.lock().expect("fake timeout")
}

pub fn invocations() -> usize {
    state().invocations.load(Ordering::SeqCst)
}

pub fn enabled() -> bool {
    STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|state| state.enabled.load(Ordering::SeqCst))
    })
}

/// The state a lane has to adopt to stand in for the thread that started it.
pub fn export() -> Option<Arc<FakeState>> {
    STATE.with(|cell| cell.borrow().clone())
}

pub fn adopt(state: Option<Arc<FakeState>>) {
    STATE.with(|cell| *cell.borrow_mut() = state);
}

pub fn next_outcome(request: &EvalRunRequest) -> EvalRunOutcome {
    let state = state();
    *state.last_timeout_secs.lock().expect("fake timeout") = request.timeout_secs;
    let count = state.invocations.fetch_add(1, Ordering::SeqCst) + 1;
    enter_lane(&state);

    std::fs::create_dir_all(request.workspace_dir).expect("workspace dir");
    let outputs_dir = request.workspace_dir.join(crate::agentskills::outputs::OUTPUTS_DIR);
    if std::fs::create_dir_all(&outputs_dir).is_ok() {
        let _ = std::fs::write(outputs_dir.join("out.json"), r#"{"ok":true}"#);
    }
    std::fs::write(request.transcript_path, format!(r#"{{"invocation":{count}}}"#)).expect("transcript");

    if state
        .tamper_case
        .lock()
        .expect("fake tamper")
        .as_deref()
        .is_some_and(|case| case == request.eval.id.as_str())
    {
        let skill_md = request.skill_path.join("SKILL.md");
        let mut content = std::fs::read_to_string(&skill_md).expect("skill md");
        content.push_str("\nrewritten by the run\n");
        std::fs::write(&skill_md, content).expect("skill md");
    }

    if state
        .remove_case
        .lock()
        .expect("fake remove")
        .as_deref()
        .is_some_and(|case| case == request.eval.id.as_str())
    {
        std::fs::remove_dir_all(request.skill_path).expect("skill dir");
    }

    leave_lane(&state);

    let cost = state
        .cost_usd
        .lock()
        .expect("fake cost")
        .map(|usd| RunCost::Priced { usd });

    let transient = state
        .transient_failures
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
        .is_ok();
    if state.log_mock_calls.load(Ordering::SeqCst) {
        let run_dir = request.transcript_path.parent().expect("run dir");
        let violations = match transient {
            true => {
                r#"[{"server":"issues","tool":"create_issue","path":"repo","constraint":"equals acme/repo","received":"acme/other"}]"#
            }
            false => "[]",
        };
        let path = run_dir.join(crate::agentskills::mocks::MOCK_CALLS_LOG_NAME);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("mock call log");
        writeln!(
            file,
            r#"{{"server":"issues","tool":"create_issue","input":{{"repo":"acme/repo"}},"violations":{violations}}}"#
        )
        .expect("mock call log");
    }

    if transient {
        return EvalRunOutcome {
            status: RunStatus::Failed,
            failure_kind: Some(crate::agentskills::runner::FAILURE_KIND_RUNNER),
            duration_ms: count as u64 * 100,
            exit_code: Some(1),
            tokens: HarnessTokenUsage::reported(Some(count as u64), Some(0), None),
            cost: cost.clone(),
            final_text: format!("transient-{count}"),
            read_only_fixture_violations: Vec::new(),
        };
    }

    EvalRunOutcome {
        status: RunStatus::Completed,
        failure_kind: None,
        duration_ms: count as u64 * 100,
        exit_code: Some(0),
        tokens: HarnessTokenUsage::reported(Some(count as u64), Some(0), None),
        cost,
        final_text: format!("run-{count}"),
        read_only_fixture_violations: Vec::new(),
    }
}

fn enter_lane(state: &FakeState) {
    let mut census = state.lanes.lock().expect("fake lanes");
    census.in_flight += 1;
    census.peak = census.peak.max(census.in_flight);
    if census.in_flight >= census.expected {
        state.joined.notify_all();
        return;
    }
    let (guard, _) = state
        .joined
        .wait_timeout_while(census, LANE_RENDEZVOUS, |census| census.in_flight < census.expected)
        .expect("fake lanes");
    drop(guard);
}

fn leave_lane(state: &FakeState) {
    let mut census = state.lanes.lock().expect("fake lanes");
    census.in_flight -= 1;
}
