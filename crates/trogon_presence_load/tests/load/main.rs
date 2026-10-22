//! BUILD-LOAD measurement harness. Brings up the authenticated four-account fixture against the
//! nats-server named by `TROGON_PRESENCE_NATS_SERVER`, measures auth callout admission and the
//! service profile, prints the tables and, when `TROGON_PRESENCE_LOAD_OUT` names a directory,
//! writes the raw results there as JSON. It is a local single-node measurement and asserts no
//! capacity; it fails only when the harness itself cannot complete.

#[path = "../../../trogon_presence_auth/tests/common/mod.rs"]
mod common;
#[path = "../../../trogon_presence_e2e/tests/support/mod.rs"]
mod support;

mod auth;
mod recorder;
mod rss;
mod service;
mod stats;

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use common::{BoxError, TestResult};
use stats::Table;

const OUT_ENV: &str = "TROGON_PRESENCE_LOAD_OUT";

fn print(tables: &[Table]) {
    for table in tables {
        println!("{}", table.render());
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "load measurement, run through `mise run presence:load`"]
async fn measure_auth_and_service_load() -> TestResult {
    let capture = recorder::install();
    let started = std::time::Instant::now();
    let load_before = rss::load_average();
    let mut version = None;
    let Some(auth) = auth::measure(&capture, &mut version).await? else {
        return Ok(());
    };
    let auth_wall = started.elapsed();
    let Some(service) = service::measure(&capture).await? else {
        return Ok(());
    };
    let total = started.elapsed();
    let version = version.unwrap_or_default();
    println!("\npresence load measurement against nats-server {version}");
    print(&auth.tables);
    print(&service.tables);
    println!(
        "\nauth phase {:.1} s, service phase {:.1} s, {} with load average before {load_before} after {}",
        auth_wall.as_secs_f64(),
        (total - auth_wall).as_secs_f64(),
        rss::host_label(&load_before),
        rss::load_average()
    );
    let finished = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let report = json!({
        "nats_server": version,
        "finished_unix_s": finished.as_secs(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        "host": rss::host_label(&load_before),
        "load_average_before": load_before,
        "load_average_after": rss::load_average(),
        "worker_threads": std::thread::available_parallelism().map_or(0, |cores| cores.get()),
        "auth_wall_s": stats::round(auth_wall.as_secs_f64()),
        "service_wall_s": stats::round((total - auth_wall).as_secs_f64()),
        "auth": auth.json,
        "service": service.json,
    });
    if let Some(dir) = std::env::var_os(OUT_ENV).map(PathBuf::from) {
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("load-{version}-{}.json", finished.as_secs()));
        std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
        println!("results written to {}", path.display());
    }
    Ok::<_, BoxError>(())
}
