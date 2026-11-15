#![allow(
    unknown_lints,
    dead_code,
    reason = "the normal compiler does not load the policy library"
)]
#![allow(
    debug_remnants,
    telemetry_attribute_literal,
    unstructured_log_fields,
    reason = "this fixture isolates typed secret exposure at diagnostic sinks"
)]

use secrecy::{ExposeSecret, SecretString};
use tracing::info as record_event;

fn traced(secret: &SecretString) {
    tracing::info!(credential = secret.expose_secret());
}

fn aliased(secret: &SecretString) {
    record_event!(credential = secret.expose_secret());
}

struct LocallyWrappedSecret(String);

impl ExposeSecret<str> for LocallyWrappedSecret {
    fn expose_secret(&self) -> &str {
        &self.0
    }
}

fn local_implementation(secret: &LocallyWrappedSecret) {
    tracing::info!(credential = secret.expose_secret());
}

fn printed(secret: &SecretString) {
    println!("{}", secret.expose_secret());
}

#[allow(
    clippy::panic,
    reason = "this compile-only fixture exercises the panic diagnostic sink"
)]
fn panicked(secret: &SecretString) {
    panic!("{}", ExposeSecret::expose_secret(secret));
}

fn authorized_header(secret: &SecretString) -> String {
    format!("Bearer {}", secret.expose_secret())
}

fn redacted(secret: &SecretString) {
    tracing::info!(credential = ?secret);
}

fn safe_metadata(secret: &SecretString) {
    tracing::info!(credential_is_empty = secret.expose_secret().is_empty());
}

struct PublicText;

impl PublicText {
    fn expose_secret(&self) -> &str {
        ""
    }
}

fn unrelated(value: &PublicText) {
    tracing::info!(public_value = value.expose_secret());
}

#[expect(
    secret_exposure_in_diagnostics,
    reason = "this compile-only case verifies deliberate opt-outs"
)]
fn deliberate(secret: &SecretString) {
    eprintln!("{}", secret.expose_secret());
}

fn main() {}
