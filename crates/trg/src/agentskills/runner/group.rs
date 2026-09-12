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
///
/// The group is a background one as far as the terminal is concerned, which is why the
/// caller hands the harness a closed stdin: a background process that reads the
/// controlling terminal is stopped with `SIGTTIN` rather than given the keystrokes, and a
/// stopped harness is indistinguishable from a working one to anything watching it exit.
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
        self.take_down();
    }

    /// The harness exited on its own; take down whatever it left behind.
    ///
    /// A harness that finishes can still leave tools running, and they keep writing
    /// into a workspace that is about to be scored and holding the write end of the
    /// pipes this run is read through, so nothing here is free to outlive the run.
    ///
    /// A group exists for as long as it has a member, and its identifier is the
    /// identifier of the process that led it, so an identifier that answers here is
    /// still this run's group and not something unrelated that inherited the number.
    pub fn stop_leftovers(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        if group_exists(self.pgid) {
            self.take_down();
        }
    }

    fn take_down(&self) {
        signal_group(self.pgid, libc::SIGTERM);
        thread::sleep(TERMINATION_GRACE);
        signal_group(self.pgid, libc::SIGKILL);
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

/// Whether any process still belongs to this group.
fn group_exists(pgid: i32) -> bool {
    if pgid <= 0 {
        return false;
    }
    unsafe { libc::kill(-pgid, 0) == 0 }
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
            handle_unless_ignored(signal);
        }
    });
}

/// Install the handler for one signal, unless this process was started with that signal
/// already ignored.
///
/// `nohup`, a launch from a job runner, and a background shell all answer some of these
/// signals with "ignore" on the process's behalf. A handler installed over that answer
/// turns the arrangement inside out: a hangup the operator arranged to survive would
/// instead kill every harness and then `trg` itself, which is the opposite of what asking
/// for it to be ignored meant.
fn handle_unless_ignored(signal: i32) {
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(signal, std::ptr::null(), &mut current) != 0 {
            return;
        }
        if current.sa_sigaction == libc::SIG_IGN {
            return;
        }
        let mut wanted: libc::sigaction = std::mem::zeroed();
        wanted.sa_sigaction = on_termination as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut wanted.sa_mask);
        libc::sigaction(signal, &wanted, std::ptr::null_mut());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn disposition(signal: i32) -> libc::sighandler_t {
        let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(signal, std::ptr::null(), &mut current) },
            0,
            "sigaction query"
        );
        current.sa_sigaction
    }

    fn set_disposition(signal: i32, handler: libc::sighandler_t) {
        let mut wanted: libc::sigaction = unsafe { std::mem::zeroed() };
        wanted.sa_sigaction = handler;
        unsafe {
            libc::sigemptyset(&mut wanted.sa_mask);
            assert_eq!(
                libc::sigaction(signal, &wanted, std::ptr::null_mut()),
                0,
                "sigaction set"
            );
        }
    }

    /// `nohup trg ...` asks for a hangup to be ignored. Answering it with a handler that
    /// kills every harness and re-raises is the opposite of what was asked for.
    #[test]
    fn a_signal_the_process_was_told_to_ignore_stays_ignored() {
        let restore = disposition(libc::SIGHUP);

        set_disposition(libc::SIGHUP, libc::SIG_IGN);
        handle_unless_ignored(libc::SIGHUP);
        let over_ignored = disposition(libc::SIGHUP);

        set_disposition(libc::SIGHUP, libc::SIG_DFL);
        handle_unless_ignored(libc::SIGHUP);
        let over_default = disposition(libc::SIGHUP);

        set_disposition(libc::SIGHUP, restore);

        assert_eq!(over_ignored, libc::SIG_IGN, "an ignored hangup must be left ignored");
        assert_eq!(
            over_default, on_termination as *const () as libc::sighandler_t,
            "a hangup nobody spoke for still has to take the harnesses down"
        );
    }
}
