use super::*;
use std::io::BufRead;
use std::os::unix::process::ExitStatusExt;

fn disposition(signal: i32) -> libc::sighandler_t {
    // SAFETY: `sigaction` is a plain C struct for which all zeroes is a valid value.
    let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
    assert_eq!(
        // SAFETY: `current` is a live local, and a null new-action pointer only queries the disposition.
        unsafe { libc::sigaction(signal, std::ptr::null(), &mut current) },
        0,
        "sigaction query"
    );
    current.sa_sigaction
}

fn set_disposition(signal: i32, handler: libc::sighandler_t) {
    // SAFETY: `sigaction` is a plain C struct for which all zeroes is a valid value.
    let mut wanted: libc::sigaction = unsafe { std::mem::zeroed() };
    wanted.sa_sigaction = handler;
    // SAFETY: `wanted` is a live, initialized `sigaction`, and a null old-action pointer is accepted.
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

    // SAFETY: `kill` takes no pointers, and the target is the child this test spawned.
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
