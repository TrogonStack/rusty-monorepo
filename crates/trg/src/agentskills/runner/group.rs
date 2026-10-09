//! Signal delivery to the whole process tree a harness subprocess leads.
//!
//! A harness spawns tools, and those tools spawn children of their own. Killing only the
//! harness leaves that tree running: it keeps consuming the model provider's quota and
//! this machine's CPU after the run is over, it keeps writing into a workspace that is
//! about to be scored, and because a grandchild still holds the write end of the captured
//! pipes the reader threads never see end of file, so the timeout that was supposed to
//! bound the run never returns at all.

use crate::agentskills::exit_code::TerminationSignal;
use crate::telemetry::INTERRUPT_FLUSH_TIMEOUT;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
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

/// Set by the first termination signal. A second one, or one landing while the first is
/// still flushing, takes the process down at once instead of waiting again.
static TERMINATING: AtomicBool = AtomicBool::new(false);

/// The write end of the pipe that wakes the flush watcher, and the read end it answers
/// on once telemetry is flushed. `-1` until the watcher is running; without it the
/// handler re-raises straight away, as it always did.
static WAKE_WATCHER: AtomicI32 = AtomicI32::new(-1);
static WATCHER_DONE: AtomicI32 = AtomicI32::new(-1);

/// How long the handler holds the signal back for the watcher before re-raising it
/// regardless, so an unreachable collector or a lock the interrupted thread holds can
/// delay the exit but never prevent it.
const FLUSH_WAIT: Duration = Duration::from_millis(INTERRUPT_FLUSH_TIMEOUT.as_millis() as u64 + 500);

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
    let mut wake = unsafe { File::from_raw_fd(wake_read) };
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
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    for fd in fds {
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            close_all(&fds);
            return None;
        }
    }
    Some((fds[0], fds[1]))
}

fn close_all(fds: &[i32]) {
    for &fd in fds {
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
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
    }
    if !TERMINATING.swap(true, Ordering::SeqCst) {
        wait_for_flush(signal);
    }
    // Hand the signal back to the default disposition so the exit status still reports
    // what actually happened, which is `ExitCode::Interrupted` as a caller reads it.
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
    if unsafe { libc::write(wake, std::ptr::addr_of!(byte).cast(), 1) } != 1 {
        return;
    }
    let mut answer = libc::pollfd {
        fd: done,
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut answer, 1, FLUSH_WAIT.as_millis() as libc::c_int) };
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

    const SIGNAL_CHILD: &str = "TRG_TEST_TERMINATION_CHILD";

    /// The body of [`a_termination_signal_kills_the_harnesses_and_exits_with_its_status`],
    /// run in a child copy of this test binary. A no-op in an ordinary test run.
    #[test]
    fn termination_child() {
        if std::env::var_os(SIGNAL_CHILD).is_none() {
            return;
        }
        let mut command = Command::new("sleep");
        command.arg("30");
        lead_own_group(&mut command);
        let mut child = command.spawn().expect("spawn sleep");
        let _group = ProcessGroupGuard::led_by(&child);
        println!("harness={}", child.id());
        std::io::stdout().flush().expect("flush stdout");
        let _ = child.wait();
    }

    #[test]
    fn a_termination_signal_kills_the_harnesses_and_exits_with_its_status() {
        use std::io::BufRead;
        use std::os::unix::process::ExitStatusExt;

        let mut child = Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "agentskills::runner::group::tests::termination_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SIGNAL_CHILD, "1")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn child test");
        let stdout = child.stdout.take().expect("piped stdout");
        let harness: i32 = std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .find_map(|line| line.split_once("harness=").map(|(_, pid)| pid.trim().to_string()))
            .expect("child reports its harness")
            .parse()
            .expect("a pid");

        unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
        let status = child.wait().expect("child exits");

        assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
        let gone = (0..100).any(|_| {
            if !process_is_alive(harness) {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
            false
        });
        assert!(gone, "the harness outlived the signal");
    }
}
