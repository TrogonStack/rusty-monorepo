// edition:2024
#![allow(unused, inline_module_block, reason = "this fixture isolates blocking APIs in async execution")]

use std::fs::read_to_string as read_text;
use std::thread::sleep as pause_thread;
use std::time::Duration;

async fn blocking_thread_calls() {
    std::thread::sleep(Duration::ZERO);
    std::thread::park();
    std::thread::park_timeout(Duration::ZERO);
    pause_thread(Duration::ZERO);
}

async fn blocking_filesystem_calls() {
    let _ = std::fs::read("fixture");
    let _ = read_text("fixture");
    let _ = std::fs::write("fixture", b"fixture");
}

fn synchronous_function() {
    std::thread::sleep(Duration::ZERO);
    let _ = std::fs::read("fixture");
}

fn async_block_inside_sync_function() {
    let _future = async {
        std::thread::sleep(Duration::ZERO);
    };
}

fn returned_async_block() -> impl std::future::Future<Output = ()> {
    async {
        std::thread::sleep(Duration::ZERO);
    }
}

fn async_closure() {
    let _callback = async || std::thread::sleep(Duration::ZERO);
}

async fn synchronous_closures_are_not_async_bodies() -> impl FnOnce() {
    let _stored = || std::thread::sleep(Duration::ZERO);
    let _worker = std::thread::spawn(|| std::thread::sleep(Duration::ZERO));
    || std::thread::sleep(Duration::ZERO)
}

async fn immediately_invoked_closure() {
    (|| std::thread::sleep(Duration::ZERO))();
}

mod unrelated {
    pub fn sleep(_: std::time::Duration) {}
    pub fn read(_: &str) {}
}

async fn lookalikes() {
    unrelated::sleep(Duration::ZERO);
    unrelated::read("fixture");
}

#[expect(blocking_call_in_async, reason = "the dedicated thread executor for this path permits blocking")]
async fn deliberately_blocking() {
    std::thread::sleep(Duration::ZERO);
}

fn main() {}
