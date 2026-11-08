//! Signal delivery to the whole process tree a harness subprocess leads.
//!
//! A harness spawns tools, and those tools spawn children of their own. Killing only the
//! harness leaves that tree running: it keeps consuming the model provider's quota and
//! this machine's CPU after the run is over, it keeps writing into a workspace that is
//! about to be scored, and because a grandchild still holds the write end of the captured
//! pipes the reader threads never see end of file, so the timeout that was supposed to
//! bound the run never returns at all.

use std::os::unix::process::CommandExt;
mod constants;
use crate::exit_code::TerminationSignal;
use constants::{FLUSH_WAIT, MAX_TRACKED_GROUPS, TERMINATION_GRACE};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Once;
use std::thread;
use std::time::Duration;

static TRACKED_GROUPS: [AtomicI32; MAX_TRACKED_GROUPS] = [const { AtomicI32::new(0) }; MAX_TRACKED_GROUPS];

static INSTALL_HANDLER: Once = Once::new();

/// Set by the first termination signal. A second one, or one landing while the first is
/// still flushing, takes the process down at once instead of waiting again.
static TERMINATING: AtomicBool = AtomicBool::new(false);

/// The write end of the pipe that wakes the flush watcher, and the read end it answers
/// on once telemetry is flushed. `-1` until the watcher is running; without it the
/// handler re-raises straight away, as it always did.
static WAKE_WATCHER: AtomicI32 = AtomicI32::new(-1);
static WATCHER_DONE: AtomicI32 = AtomicI32::new(-1);

/// Make the child the leader of its own process group, so its descendants can be
/// addressed as one.
///
/// The group is a background one as far as the terminal is concerned, which is why the
/// caller hands the harness a closed stdin: a background process that reads the
/// controlling terminal is stopped with `SIGTTIN` rather than given the keystrokes, and a
/// stopped harness is indistinguishable from a working one to anything watching it exit.
pub fn lead_own_group(command: &mut Command) {
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
    // SAFETY: `kill` with signal 0 only checks that the target exists and touches no memory.
    unsafe { libc::kill(-pgid, 0) == 0 }
}

fn signal_group(pgid: i32, signal: i32) {
    if pgid <= 0 {
        return;
    }
    // SAFETY: `kill` takes no pointers; a negative process identifier addresses every process in the group.
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

/// Take every running harness down before `trg` itself goes, and make sure a termination
/// signal gives telemetry a chance to flush before it does.
///
/// Without this, interrupting `trg` leaves the harnesses it started behind: they are in
/// their own process groups precisely so that a timeout can reach their children, which
/// also means the terminal's own interrupt no longer reaches them.
pub(crate) fn install_termination_handler() {
    INSTALL_HANDLER.call_once(|| {
        start_flush_watcher();
        for signal in TerminationSignal::ALL {
            handle_unless_ignored(signal.number());
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
    // SAFETY: `sigaction` is a plain C struct for which all zeroes is a valid value, and every pointer passed is either a live local or null where the call accepts null.
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

/// Runs `work` with the termination signals blocked on the calling thread, so any thread
/// it spawns inherits the block and is never picked to run the handler.
///
/// The handler holds whichever thread it lands on until the flush finishes; landing on a
/// thread the flush itself depends on, such as an exporter's worker or the watcher, would
/// only ever end in the timeout.
pub fn with_termination_signals_blocked<T>(work: impl FnOnce() -> T) -> T {
    // SAFETY: `sigset_t` is valid when zeroed, and every pointer passed is a live local.
    let previous = unsafe {
        let mut blocked: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut blocked);
        for signal in TerminationSignal::ALL {
            libc::sigaddset(&mut blocked, signal.number());
        }
        let mut previous: libc::sigset_t = std::mem::zeroed();
        let ok = libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous) == 0;
        ok.then_some(previous)
    };
    let output = work();
    if let Some(previous) = previous {
        // SAFETY: `previous` is the mask `pthread_sigmask` filled in above, and a null old-mask pointer is accepted.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) };
    }
    output
}

/// If a termination signal is being handled, never return: the handler re-raises it, and
/// the process has to exit with that signal's status rather than whatever code the work
/// it interrupted came back with in the meantime.
pub fn yield_to_termination() {
    if !TERMINATING.load(Ordering::SeqCst) {
        return;
    }
    loop {
        thread::park();
    }
}

/// Start the thread that flushes telemetry on the handler's behalf, since the handler
/// itself may do nothing but async-signal-safe work.
fn start_flush_watcher() {
    let Some((wake_read, wake_write)) = cloexec_pipe() else {
        return;
    };
    let Some((done_read, done_write)) = cloexec_pipe() else {
        close_all(&[wake_read, wake_write]);
        return;
    };
    let spawned = with_termination_signals_blocked(|| {
        thread::Builder::new()
            .name("trg-signal-flush".to_string())
            .spawn(move || watch_for_termination(wake_read, done_write))
    });
    if spawned.is_err() {
        close_all(&[wake_read, wake_write, done_read, done_write]);
        return;
    }
    WATCHER_DONE.store(done_read, Ordering::SeqCst);
    WAKE_WATCHER.store(wake_write, Ordering::SeqCst);
}

fn watch_for_termination(wake_read: i32, done_write: i32) {
    // SAFETY: `start_flush_watcher` hands this thread sole ownership of the descriptor, freshly opened by `cloexec_pipe`.
    let mut wake = unsafe { File::from_raw_fd(wake_read) };
    // SAFETY: `start_flush_watcher` hands this thread sole ownership of the descriptor, freshly opened by `cloexec_pipe`.
    let mut done = unsafe { File::from_raw_fd(done_write) };
    let mut signal = [0_u8; 1];
    if wake.read_exact(&mut signal).is_err() {
        return;
    }
    if let Some(signal) = TerminationSignal::ALL
        .into_iter()
        .find(|candidate| candidate.number() == i32::from(signal[0]))
    {
        crate::telemetry::interrupt(signal);
    }
    let _ = done.write_all(&[1]);
}

/// A pipe whose ends a harness never inherits.
fn cloexec_pipe() -> Option<(i32, i32)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors `pipe` writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    for fd in fds {
        // SAFETY: `fcntl` with `F_SETFD` takes no pointers, and `fd` was just returned by `pipe`.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            close_all(&fds);
            return None;
        }
    }
    Some((fds[0], fds[1]))
}

fn close_all(fds: &[i32]) {
    for &fd in fds {
        // SAFETY: the caller owns every descriptor in `fds` and never uses one again after it is closed here.
        unsafe { libc::close(fd) };
    }
}

/// Runs in signal context, so everything here is async-signal-safe: atomics, `kill`,
/// `write`, `poll`, `sigaction` and `raise`. The flush itself takes locks and does network
/// IO, so it happens on the watcher thread while this one waits, bounded by
/// [`FLUSH_WAIT`].
extern "C" fn on_termination(signal: i32) {
    for slot in TRACKED_GROUPS.iter() {
        let pgid = slot.load(Ordering::SeqCst);
        if pgid > 0 {
            // SAFETY: `kill` takes no pointers and is async-signal-safe.
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
    }
    if !TERMINATING.swap(true, Ordering::SeqCst) {
        wait_for_flush(signal);
    }
    // Hand the signal back to the default disposition so the exit status still reports
    // what actually happened, which is `ExitCode::Interrupted` as a caller reads it.
    // SAFETY: `sigaction` is valid when zeroed, every pointer passed is a live local or null, and each call here is async-signal-safe.
    unsafe {
        let mut default: libc::sigaction = std::mem::zeroed();
        default.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut default.sa_mask);
        libc::sigaction(signal, &default, std::ptr::null_mut());
        libc::raise(signal);
    }
}

fn wait_for_flush(signal: i32) {
    let wake = WAKE_WATCHER.load(Ordering::SeqCst);
    let done = WATCHER_DONE.load(Ordering::SeqCst);
    let Ok(byte) = u8::try_from(signal) else {
        return;
    };
    if wake < 0 || done < 0 {
        return;
    }
    // SAFETY: the pointer covers exactly the single byte of `byte`, and `write` is async-signal-safe.
    if unsafe { libc::write(wake, std::ptr::addr_of!(byte).cast(), 1) } != 1 {
        return;
    }
    let mut answer = libc::pollfd {
        fd: done,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `answer` is one live `pollfd`, matching the count of 1, and `poll` is async-signal-safe.
    unsafe { libc::poll(&mut answer, 1, FLUSH_WAIT.as_millis() as libc::c_int) };
}

#[cfg(test)]
pub fn process_is_alive(pid: i32) -> bool {
    // SAFETY: `kill` with signal 0 only checks that the target exists and touches no memory.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
