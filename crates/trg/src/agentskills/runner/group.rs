//! Signal delivery to the whole process tree a harness subprocess leads.
//!
//! A harness spawns tools, and those tools spawn children of their own. Killing only the
//! harness leaves that tree running: it keeps consuming the model provider's quota and
//! this machine's CPU after the run is over, it keeps writing into a workspace that is
//! about to be scored, and because a grandchild still holds the write end of the captured
//! pipes the reader threads never see end of file, so the timeout that was supposed to
//! bound the run never returns at all.

use std::process::{Child, Command};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Once;
use std::thread;
use std::time::Duration;

/// How long a group is given to exit on its own before it is killed outright.
///
/// Long enough for a harness to finish flushing the transcript it was in the middle of
/// writing, which is the artifact the timed-out run is diagnosed from.
const TERMINATION_GRACE: Duration = Duration::from_millis(500);

/// Process groups currently running a harness.
///
/// A fixed array of atomics rather than a lock: the termination handler runs in signal
/// context, where taking a lock the interrupted thread already holds deadlocks the
/// process. Slot value `0` means free.
const MAX_TRACKED_GROUPS: usize = 64;
static TRACKED_GROUPS: [AtomicI32; MAX_TRACKED_GROUPS] = [const { AtomicI32::new(0) }; MAX_TRACKED_GROUPS];

static INSTALL_HANDLER: Once = Once::new();

/// Make the child the leader of its own process group, so its descendants can be
/// addressed as one.
pub fn lead_own_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// The process group running one harness subprocess, terminated when this is dropped
/// unless the subprocess has already exited on its own.
#[derive(Debug)]
pub struct ProcessGroupGuard {
    pgid: i32,
    slot: Option<usize>,
    armed: bool,
}

impl ProcessGroupGuard {
    pub fn led_by(child: &Child) -> Self {
        install_termination_handler();
        let pgid = i32::try_from(child.id()).unwrap_or(0);
        Self {
            pgid,
            slot: track(pgid),
            armed: pgid > 0,
        }
    }

    /// Stop the group, giving it a chance to shut down cleanly first.
    pub fn terminate(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        signal_group(self.pgid, libc::SIGTERM);
        thread::sleep(TERMINATION_GRACE);
        signal_group(self.pgid, libc::SIGKILL);
    }

    /// The subprocess exited on its own, so there is nothing left to signal.
    ///
    /// Signalling anyway would be worse than pointless: the group is gone, and the
    /// operating system is free to have reused the identifier for something unrelated.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.terminate();
        if let Some(slot) = self.slot {
            TRACKED_GROUPS[slot].store(0, Ordering::SeqCst);
        }
    }
}

fn signal_group(pgid: i32, signal: i32) {
    if pgid <= 0 {
        return;
    }
    // A negative process identifier addresses every process in the group.
    unsafe { libc::kill(-pgid, signal) };
}

fn track(pgid: i32) -> Option<usize> {
    if pgid <= 0 {
        return None;
    }
    TRACKED_GROUPS.iter().position(|slot| {
        slot.compare_exchange(0, pgid, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    })
}

/// Take every running harness down before `trg` itself goes.
///
/// Without this, interrupting `trg` leaves the harnesses it started behind: they are in
/// their own process groups precisely so that a timeout can reach their children, which
/// also means the terminal's own interrupt no longer reaches them.
fn install_termination_handler() {
    INSTALL_HANDLER.call_once(|| {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let handler = on_termination as *const () as libc::sighandler_t;
            unsafe { libc::signal(signal, handler) };
        }
    });
}

extern "C" fn on_termination(signal: i32) {
    for slot in TRACKED_GROUPS.iter() {
        let pgid = slot.load(Ordering::SeqCst);
        if pgid > 0 {
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
    }
    // Hand the signal back to the default disposition so the exit status still reports
    // what actually happened.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

#[cfg(test)]
pub fn process_is_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}
