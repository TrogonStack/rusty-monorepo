use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const TARGET: &str = "wasm32-wasip2";

fn hooks_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("trogon_presence_hooks")
}

fn target_installed() -> bool {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|sysroot| Path::new(sysroot.trim()).join("lib/rustlib").join(TARGET).is_dir())
}

fn build(example: &str, artifact: &str) -> Option<PathBuf> {
    if !target_installed() {
        eprintln!("skipping: the {TARGET} target is not installed, run `rustup target add {TARGET}`");
        return None;
    }
    let guest = hooks_dir().join("examples").join(example);
    let target_dir = hooks_dir().join("../../target/trogon_presence_hook_example");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .arg("build")
        .arg("--release")
        .args(["--target", TARGET])
        .arg("--manifest-path")
        .arg(guest.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        // Coverage instrumentation flags have no profiler runtime on wasm32-wasip2.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status();
    match status {
        Ok(status) if status.success() => Some(target_dir.join(TARGET).join("release").join(artifact)),
        Ok(status) => panic!("building the {example} hook guest failed with {status}"),
        Err(err) => panic!("could not run cargo to build the {example} hook guest: {err}"),
    }
}

#[allow(dead_code)]
pub fn example_component() -> Option<PathBuf> {
    static BUILT: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUILT
        .get_or_init(|| build("guest", "trogon_presence_hook_example.wasm"))
        .clone()
}

#[allow(dead_code)]
pub fn socket_probe_component() -> Option<PathBuf> {
    static BUILT: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUILT
        .get_or_init(|| build("socket_probe", "trogon_presence_hook_socket_probe.wasm"))
        .clone()
}

#[allow(dead_code)]
pub fn http_probe_component() -> Option<PathBuf> {
    static BUILT: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUILT
        .get_or_init(|| build("http_probe", "trogon_presence_hook_http_probe.wasm"))
        .clone()
}
