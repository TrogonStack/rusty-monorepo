use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;

const SERVER_BINARY_ENV: &str = "TROGON_PRESENCE_NATS_SERVER";
const START_ATTEMPTS: usize = 5;
const READY_ATTEMPTS: usize = 100;
const READY_BACKOFF: Duration = Duration::from_millis(50);

pub struct NatsServer {
    child: Child,
    url: String,
    _store: TempDir,
}

impl NatsServer {
    pub async fn start() -> Option<Self> {
        let binary = match std::env::var_os(SERVER_BINARY_ENV) {
            Some(pinned) => Binary::Pinned(PathBuf::from(pinned)),
            None => match locate_binary() {
                Some(found) => Binary::Discovered(found),
                None => {
                    eprintln!(
                        "skipping: nats-server is neither on PATH nor resolvable through `mise which nats-server`"
                    );
                    return None;
                }
            },
        };
        let mut last_failure = String::new();
        for _ in 0..START_ATTEMPTS {
            match Self::launch(binary.path()).await {
                Ok(server) => return Some(server),
                Err(Launch::Retry(reason)) => last_failure = reason,
                Err(Launch::Fatal(reason)) => {
                    last_failure = reason;
                    break;
                }
            }
        }
        binary.unavailable(format_args!("{last_failure}"));
        None
    }

    async fn launch(binary: &Path) -> Result<Self, Launch> {
        let store = tempfile::tempdir().map_err(|err| Launch::Fatal(format!("no store dir: {err}")))?;
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map_err(|err| Launch::Fatal(format!("no free port: {err}")))?
            .port();
        let child = Command::new(binary)
            .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
            .arg(store.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| Launch::Fatal(format!("could not spawn {}: {err}", binary.display())))?;
        let mut server = Self {
            child,
            url: format!("nats://127.0.0.1:{port}"),
            _store: store,
        };
        for _ in 0..READY_ATTEMPTS {
            if let Ok(Some(status)) = server.child.try_wait() {
                return Err(Launch::Retry(format!(
                    "nats-server exited early on port {port} with {status}"
                )));
            }
            if let Ok(client) = async_nats::connect(&server.url).await {
                if async_nats::jetstream::new(client).query_account().await.is_ok() {
                    return Ok(server);
                }
            }
            tokio::time::sleep(READY_BACKOFF).await;
        }
        server.stop();
        Err(Launch::Retry(format!(
            "nats-server did not become ready on {}",
            server.url
        )))
    }

    pub async fn client(&self) -> async_nats::Client {
        match async_nats::connect(&self.url).await {
            Ok(client) => client,
            Err(err) => panic!("connect to {}: {err}", self.url),
        }
    }

    #[allow(dead_code)]
    pub fn pause(&self) {
        self.signal("-STOP");
    }

    #[allow(dead_code)]
    pub fn resume(&self) {
        self.signal("-CONT");
    }

    fn signal(&self, signal: &str) {
        let status = Command::new("kill")
            .args([signal, &self.child.id().to_string()])
            .status();
        assert!(
            status.as_ref().is_ok_and(|status| status.success()),
            "kill {signal} failed: {status:?}"
        );
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for NatsServer {
    fn drop(&mut self) {
        self.stop();
    }
}

enum Launch {
    Retry(String),
    Fatal(String),
}

enum Binary {
    Pinned(PathBuf),
    Discovered(PathBuf),
}

impl Binary {
    fn path(&self) -> &PathBuf {
        match self {
            Self::Pinned(path) | Self::Discovered(path) => path,
        }
    }

    fn unavailable(&self, reason: std::fmt::Arguments<'_>) {
        match self {
            Self::Pinned(path) => panic!("{SERVER_BINARY_ENV}={} is unusable: {reason}", path.display()),
            Self::Discovered(_) => eprintln!("skipping: {reason}"),
        }
    }
}

fn locate_binary() -> Option<PathBuf> {
    let on_path = Command::new("nats-server")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if on_path {
        return Some(PathBuf::from("nats-server"));
    }
    let output = Command::new("mise").args(["which", "nats-server"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    Some(PathBuf::from(path.trim()))
}
