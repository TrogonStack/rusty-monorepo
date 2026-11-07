#![allow(
    unknown_lints,
    unused,
    reason = "the normal compiler does not load the policy library"
)]

use std::mem::drop as release;
use tokio::task::JoinHandle;

async fn bare_spawn() {
    tokio::spawn(async {});
}

fn bare_blocking_spawn() {
    tokio::task::spawn_blocking(|| {});
}

fn dropped_handle(handle: JoinHandle<()>) {
    std::mem::drop(handle);
}

fn dropped_new_handle() {
    drop(tokio::spawn(async {}));
}

#[allow(
    clippy::let_and_return,
    reason = "this fixture keeps binding ownership distinct from a direct return"
)]
fn owned_handle() -> JoinHandle<()> {
    let handle = tokio::spawn(async {});
    handle
}

fn returned_handle() -> JoinHandle<()> {
    tokio::spawn(async {})
}

async fn awaited_handle() {
    let _ = tokio::spawn(async {}).await;
}

#[allow(discarded_task_handle, reason = "the runtime owns this task until shutdown")]
fn explicit_detachment() {
    tokio::spawn(async {});
}

struct ApplicationHandle;

#[allow(
    clippy::drop_non_drop,
    reason = "this fixture verifies that a non-task value is not reported"
)]
fn unrelated_handle() {
    drop(ApplicationHandle);
}

fn adopt(handle: JoinHandle<()>) -> JoinHandle<()> {
    handle
}

fn adopted_handle() -> JoinHandle<()> {
    adopt(tokio::spawn(async {}))
}

fn dropped_alias_handle(handle: JoinHandle<usize>) {
    release(handle);
}

#[allow(
    dropping_references,
    reason = "dropping a reference must not count as detaching its task"
)]
fn borrowed_handle(handle: JoinHandle<()>) -> JoinHandle<()> {
    drop(&handle);
    handle
}

fn custom_drop_returns_handle(handle: JoinHandle<()>) -> JoinHandle<()> {
    fn drop(handle: JoinHandle<()>) -> JoinHandle<()> {
        handle
    }
    drop(handle)
}

fn bare_local_spawn() {
    tokio::task::spawn_local(async {});
}

#[expect(discarded_task_handle, reason = "the fixture deliberately documents detachment")]
fn expected_detachment() {
    tokio::spawn(async {});
}

fn main() {}
