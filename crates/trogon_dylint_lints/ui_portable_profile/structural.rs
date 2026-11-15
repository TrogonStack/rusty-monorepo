// edition:2024
#![deny(unfulfilled_lint_expectations)]

#[expect(unbounded_channel, reason = "portable adoption retains generic backpressure policy")]
fn main() {
    let _ = std::sync::mpsc::channel::<()>();
}
