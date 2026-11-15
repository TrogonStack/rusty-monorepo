#![allow(
    unknown_lints,
    unexpected_cfgs,
    dead_code,
    reason = "this fixture compiles with and without the policy driver"
)]
#![cfg_attr(dylint_lib = "trogon_dylint_lints", deny(unfulfilled_lint_expectations))]

#[cfg_attr(
    not(portable_profile),
    expect(
        std_env_access,
        reason = "the default profile requires the Trogon environment adapter"
    )
)]
fn environment() {
    let _ = std::env::var("APPLICATION_MODE");
}

#[cfg_attr(
    not(portable_profile),
    expect(
        telemetry_attribute_literal,
        reason = "the default profile requires generated telemetry attributes"
    )
)]
fn telemetry(span: &tracing::Span) {
    span.record("unregistered.attribute", "value");
}

#[expect(
    unbounded_channel,
    reason = "generic backpressure checks remain active in both profiles"
)]
fn structural() {
    let _ = std::sync::mpsc::channel::<()>();
}

fn main() {}
