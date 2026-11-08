use crate::telemetry::testing::CapturedTrace;

/// Fails if any of `needles` appears anywhere a span could carry it: its
/// name, an attribute, an event, or its status.
#[track_caller]
pub(crate) fn assert_never_recorded(trace: &CapturedTrace, needles: &[&str]) {
    assert!(!trace.spans.is_empty(), "nothing was captured to scan");
    for span in &trace.spans {
        let mut carried = vec![span.name.to_string(), format!("{:?}", span.status)];
        for kv in &span.attributes {
            carried.push(kv.key.to_string());
            carried.push(kv.value.to_string());
        }
        for event in span.events.iter() {
            carried.push(event.name.to_string());
            for kv in &event.attributes {
                carried.push(kv.key.to_string());
                carried.push(kv.value.to_string());
            }
        }
        for needle in needles {
            for text in &carried {
                assert!(
                    !text.contains(needle),
                    "span {:?} carries {needle:?} in {text:?}",
                    span.name
                );
            }
        }
    }
}

pub(crate) fn current_thread<F: std::future::Future>(work: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(work)
}
