use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_nats::connection::State;
use tokio::task::JoinHandle;

use tempfile::TempDir;

const SERVER_BINARY_ENV: &str = "TROGON_PRESENCE_NATS_SERVER";
const START_ATTEMPTS: usize = 5;
const READY_ATTEMPTS: usize = 100;
const READY_BACKOFF: Duration = Duration::from_millis(50);
const LINK_RECONNECT_DELAY: Duration = Duration::from_millis(50);
const LINK_STATE_TIMEOUT: Duration = Duration::from_secs(10);
const LINK_STATE_POLL: Duration = Duration::from_millis(10);

pub struct NatsServer {
    child: Child,
    address: SocketAddr,
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
        let address = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map_err(|err| Launch::Fatal(format!("no free port: {err}")))?;
        let port = address.port();
        let child = Command::new(binary)
            .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
            .arg(store.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| Launch::Fatal(format!("could not spawn {}: {err}", binary.display())))?;
        let mut server = Self {
            child,
            address,
            url: format!("nats://{address}"),
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
    pub async fn severable_link(&self) -> SeverableLink {
        SeverableLink::open(self.address).await
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

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    BothSides,
    ClientSideOnly,
}

#[allow(dead_code)]
pub struct SeverableLink {
    address: SocketAddr,
    cut: Arc<tokio::sync::watch::Sender<Option<Cut>>>,
    held: Arc<Mutex<Vec<tokio::net::TcpStream>>>,
    accept: JoinHandle<()>,
}

#[allow(dead_code)]
impl SeverableLink {
    async fn open(upstream: SocketAddr) -> Self {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) => panic!("bind the severable link: {err}"),
        };
        let address = match listener.local_addr() {
            Ok(address) => address,
            Err(err) => panic!("severable link address: {err}"),
        };
        let (cut, _) = tokio::sync::watch::channel(None);
        let cut = Arc::new(cut);
        let held: Arc<Mutex<Vec<tokio::net::TcpStream>>> = Arc::default();
        let accept = tokio::spawn(Self::accept(listener, upstream, cut.clone(), held.clone()));
        Self {
            address,
            cut,
            held,
            accept,
        }
    }

    async fn accept(
        listener: tokio::net::TcpListener,
        upstream: SocketAddr,
        cut: Arc<tokio::sync::watch::Sender<Option<Cut>>>,
        held: Arc<Mutex<Vec<tokio::net::TcpStream>>>,
    ) {
        while let Ok((inbound, _)) = listener.accept().await {
            if cut.borrow().is_some() {
                continue;
            }
            let Ok(outbound) = tokio::net::TcpStream::connect(upstream).await else {
                continue;
            };
            tokio::spawn(Self::relay(inbound, outbound, cut.subscribe(), held.clone()));
        }
    }

    async fn relay(
        mut inbound: tokio::net::TcpStream,
        mut outbound: tokio::net::TcpStream,
        mut cut: tokio::sync::watch::Receiver<Option<Cut>>,
        held: Arc<Mutex<Vec<tokio::net::TcpStream>>>,
    ) {
        let severed = tokio::select! {
            _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => None,
            severed = cut.wait_for(Option::is_some) => severed.ok().and_then(|severed| *severed),
        };
        if severed == Some(Cut::ClientSideOnly) {
            held.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(outbound);
        }
    }

    pub async fn client(&self) -> async_nats::Client {
        let url = format!("nats://{}", self.address);
        match async_nats::ConnectOptions::new()
            .reconnect_delay_callback(|_| LINK_RECONNECT_DELAY)
            .connect(&url)
            .await
        {
            Ok(client) => client,
            Err(err) => panic!("connect through the severable link {url}: {err}"),
        }
    }

    pub fn sever(&self, cut: Cut) {
        self.cut.send_replace(Some(cut));
    }

    pub fn restore(&self) {
        self.cut.send_replace(None);
    }

    pub async fn reached(client: &async_nats::Client, state: State) {
        let deadline = tokio::time::Instant::now() + LINK_STATE_TIMEOUT;
        while client.connection_state() != state {
            assert!(
                tokio::time::Instant::now() < deadline,
                "client never reached {state:?}, still {:?}",
                client.connection_state()
            );
            tokio::time::sleep(LINK_STATE_POLL).await;
        }
    }
}

impl Drop for SeverableLink {
    fn drop(&mut self) {
        self.cut.send_replace(Some(Cut::BothSides));
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.accept.abort();
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
