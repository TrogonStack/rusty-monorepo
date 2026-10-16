//! Resident set size of this test process, which hosts the service, the callout and the clients.
//! macOS reads `proc_pidinfo(PROC_PIDTASKINFO)` from libproc; Linux reads `/proc/self/statm`.

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rss(u64);

impl Rss {
    pub fn mib(self) -> f64 {
        crate::stats::round(self.0 as f64 / (1024.0 * 1024.0))
    }

    pub fn to_json(self) -> Value {
        json!(self.mib())
    }

    pub fn sample() -> Option<Self> {
        resident().map(Self)
    }
}

pub const SOURCE: &str = if cfg!(target_os = "macos") {
    "libproc proc_pidinfo(PROC_PIDTASKINFO).pti_resident_size"
} else {
    "/proc/self/statm resident pages times page size"
};

#[cfg(target_os = "macos")]
fn resident() -> Option<u64> {
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
    let pid = libc::c_int::try_from(std::process::id()).ok()?;
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            (&mut info as *mut libc::proc_taskinfo).cast(),
            size,
        )
    };
    (read == size).then_some(info.pti_resident_size)
}

#[cfg(target_os = "linux")]
fn resident() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(page).ok().map(|page| page * pages)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resident() -> Option<u64> {
    None
}

#[derive(Debug, Default)]
pub struct RssSeries(Vec<Rss>);

impl RssSeries {
    pub fn push(&mut self, sample: Rss) {
        self.0.push(sample);
    }

    pub fn to_json(&self) -> Value {
        let count = self.0.len();
        let peak = self.0.iter().max().map(|rss| rss.mib());
        let mean =
            (count > 0).then(|| crate::stats::round(self.0.iter().map(|rss| rss.mib()).sum::<f64>() / count as f64));
        json!({ "samples": count, "peak_mib": peak, "mean_mib": mean })
    }
}

/// One, five and fifteen minute system load averages, recorded so a contended run is visible in its result.
pub fn load_average() -> Value {
    let mut loads = [0.0_f64; 3];
    let read = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
    if read == 3 {
        json!(loads.map(crate::stats::round))
    } else {
        Value::Null
    }
}

/// One minute load average at or above which a run is labelled as measured on a noisy host.
pub const QUIET_LOAD: f64 = 6.0;

/// "quiet" when the one minute load average was below [`QUIET_LOAD`], otherwise "noisy host".
pub fn host_label(load: &Value) -> &'static str {
    match load.get(0).and_then(Value::as_f64) {
        Some(one_minute) if one_minute < QUIET_LOAD => "quiet",
        Some(_) => "noisy host",
        None => "unknown",
    }
}
