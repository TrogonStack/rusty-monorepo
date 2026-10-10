//! A process-wide `metrics` recorder that captures the callout's own decision counter and
//! duration histogram, so callout latency is the callout's measurement, not the client's.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use metrics::{
    Counter, CounterFn, Gauge, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};
use trogon_presence_auth::telemetry::{DECISIONS, DURATION, ERROR_TYPE_ATTRIBUTE, OUTCOME_ATTRIBUTE};

use crate::stats::{Samples, Tally};

#[derive(Debug, Default)]
pub struct Capture {
    decisions: Mutex<Tally>,
    durations: Mutex<BTreeMap<String, Vec<f64>>>,
}

/// What the callout decided over one measurement window.
#[derive(Debug, Default)]
pub struct CalloutWindow {
    pub decisions: Tally,
    pub durations: BTreeMap<String, Samples>,
}

impl CalloutWindow {
    pub fn all_durations(&self) -> Samples {
        let mut all = Samples::default();
        for samples in self.durations.values() {
            all.merge(samples.clone());
        }
        all
    }

    pub fn durations_of(&self, outcome: &str) -> Samples {
        self.durations.get(outcome).cloned().unwrap_or_default()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Capture {
    /// Drains everything recorded since the previous call.
    pub fn take(&self) -> CalloutWindow {
        let decisions = std::mem::take(&mut *lock(&self.decisions));
        let durations = std::mem::take(&mut *lock(&self.durations))
            .into_iter()
            .map(|(outcome, seconds)| {
                let samples = seconds
                    .into_iter()
                    .filter_map(|value| Duration::try_from_secs_f64(value).ok())
                    .collect();
                (outcome, samples)
            })
            .collect();
        CalloutWindow { decisions, durations }
    }
}

fn label<'a>(key: &'a Key, name: &str) -> &'a str {
    key.labels()
        .find(|label| label.key() == name)
        .map_or("", |label| label.value())
}

struct DecisionCounter {
    capture: Arc<Capture>,
    decision: String,
}

impl CounterFn for DecisionCounter {
    fn increment(&self, value: u64) {
        lock(&self.capture.decisions).add(self.decision.clone(), value);
    }

    fn absolute(&self, _value: u64) {}
}

struct DurationHistogram {
    capture: Arc<Capture>,
    outcome: String,
}

impl HistogramFn for DurationHistogram {
    fn record(&self, value: f64) {
        lock(&self.capture.durations)
            .entry(self.outcome.clone())
            .or_default()
            .push(value);
    }
}

struct CaptureRecorder(Arc<Capture>);

impl Recorder for CaptureRecorder {
    fn describe_counter(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_gauge(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_histogram(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn register_counter(&self, key: &Key, _metadata: &Metadata<'_>) -> Counter {
        if key.name() != DECISIONS {
            return Counter::noop();
        }
        let outcome = label(key, OUTCOME_ATTRIBUTE);
        let decision = match label(key, ERROR_TYPE_ATTRIBUTE) {
            "" => outcome.to_owned(),
            error => format!("{outcome}:{error}"),
        };
        Counter::from_arc(Arc::new(DecisionCounter {
            capture: self.0.clone(),
            decision,
        }))
    }

    fn register_gauge(&self, _key: &Key, _metadata: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }

    fn register_histogram(&self, key: &Key, _metadata: &Metadata<'_>) -> Histogram {
        if key.name() != DURATION {
            return Histogram::noop();
        }
        Histogram::from_arc(Arc::new(DurationHistogram {
            capture: self.0.clone(),
            outcome: label(key, OUTCOME_ATTRIBUTE).to_owned(),
        }))
    }
}

static CAPTURE: OnceLock<Arc<Capture>> = OnceLock::new();

/// Installs the capturing recorder once per process and returns its shared capture.
pub fn install() -> Arc<Capture> {
    CAPTURE
        .get_or_init(|| {
            let capture = Arc::new(Capture::default());
            let _ = metrics::set_global_recorder(CaptureRecorder(capture.clone()));
            capture
        })
        .clone()
}
