pub mod domain;

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use async_nats::Message;
use futures_util::StreamExt;
use serde_json::Value;
use tempfile::TempDir;
use trogon_presence::{HolderId, PresenceKey};
use trogon_presence_service::reply::HEADER_CODE;
use trogon_presence_service::{ServiceHandle, CALLER_INBOX_PREFIX};

const SERVER_BINARY_ENV: &str = "TROGON_PRESENCE_NATS_SERVER";
const START_ATTEMPTS: usize = 5;
const READY_ATTEMPTS: usize = 100;
const READY_BACKOFF: Duration = Duration::from_millis(50);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[allow(dead_code)]
pub fn caller_inbox(key: &PresenceKey) -> Result<String, BoxError> {
    Ok(format!(
        "{CALLER_INBOX_PREFIX}.{}.c1.{}",
        key.token(),
        HolderId::generate()?
    ))
}

#[allow(dead_code)]
pub async fn command(
    client: &async_nats::Client,
    subject: String,
    key: &PresenceKey,
    body: &Value,
) -> Result<Message, BoxError> {
    command_via(client, subject, caller_inbox(key)?, body).await
}

#[allow(dead_code)]
pub async fn command_via(
    client: &async_nats::Client,
    subject: String,
    inbox: String,
    body: &Value,
) -> Result<Message, BoxError> {
    let mut replies = client.subscribe(inbox.clone()).await?;
    client
        .publish_with_reply(subject, inbox, serde_json::to_vec(body)?.into())
        .await?;
    client.flush().await?;
    tokio::time::timeout(COMMAND_TIMEOUT, replies.next())
        .await
        .map_err(|_| "no command reply")?
        .ok_or_else(|| "reply subscription ended".into())
}

#[allow(dead_code)]
pub fn code_of(message: &Message) -> Option<&str> {
    message
        .headers
        .as_ref()
        .and_then(|headers| headers.get(HEADER_CODE))
        .map(|value| value.as_str())
}

#[allow(dead_code)]
pub fn body_of(message: &Message) -> Result<Value, BoxError> {
    Ok(serde_json::from_slice(&message.payload)?)
}

#[allow(dead_code)]
pub async fn wait_for_writers(handle: &ServiceHandle, expected: usize, within: Duration) -> Result<(), BoxError> {
    tokio::time::timeout(within, async {
        while handle.writer_shards().len() != expected {
            tokio::time::sleep(POLL).await;
        }
    })
    .await
    .map_err(|_| {
        format!(
            "held {} writer shards, expected {expected}",
            handle.writer_shards().len()
        )
        .into()
    })
}

pub struct NatsServer {
    child: Child,
    url: String,
    access: Access,
    port: u16,
    _store: TempDir,
}

/// Who may connect to a test server and what the service's runtime user may publish.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub enum Access {
    Open,
    /// The runtime user may publish anywhere except the plain `_INBOX.>` space, mirroring the
    /// least-privilege runtime grant a deployment hands the service. Callers connect as a second,
    /// unrestricted user.
    RuntimeDeniedPlainInbox,
}

const RUNTIME_USER: &str = "runtime";
const CALLER_USER: &str = "caller";

impl Access {
    fn config(self, port: u16, store: &Path) -> Option<String> {
        match self {
            Self::Open => None,
            Self::RuntimeDeniedPlainInbox => Some(format!(
                "listen: \"127.0.0.1:{port}\"\njetstream {{\n  store_dir: \"{store}\"\n}}\nauthorization {{\n  users: [\n    {{ user: {RUNTIME_USER}, password: {RUNTIME_USER}, permissions: {{ publish: {{ allow: [\">\"], deny: [\"_INBOX.>\"] }}, subscribe: {{ allow: [\">\"] }} }} }}\n    {{ user: {CALLER_USER}, password: {CALLER_USER} }}\n  ]\n}}\n",
                store = store.display(),
            )),
        }
    }

    async fn connect(self, port: u16, user: &str) -> Result<async_nats::Client, async_nats::ConnectError> {
        let address = format!("nats://127.0.0.1:{port}");
        match self {
            Self::Open => async_nats::connect(address).await,
            Self::RuntimeDeniedPlainInbox => {
                async_nats::ConnectOptions::with_user_and_password(user.to_owned(), user.to_owned())
                    .connect(address)
                    .await
            }
        }
    }
}

impl NatsServer {
    #[allow(dead_code)]
    pub async fn start() -> Option<Self> {
        Self::start_with(Access::Open).await
    }

    #[allow(dead_code)]
    pub async fn start_with(access: Access) -> Option<Self> {
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
            match Self::launch(binary.path(), access).await {
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

    async fn launch(binary: &Path, access: Access) -> Result<Self, Launch> {
        let store = tempfile::tempdir().map_err(|err| Launch::Fatal(format!("no store dir: {err}")))?;
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map_err(|err| Launch::Fatal(format!("no free port: {err}")))?
            .port();
        let mut command = Command::new(binary);
        match access.config(port, store.path()) {
            Some(config) => {
                let path = store.path().join("server.conf");
                std::fs::write(&path, config).map_err(|err| Launch::Fatal(format!("no server config: {err}")))?;
                command.arg("-c").arg(path);
            }
            None => {
                command
                    .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
                    .arg(store.path());
            }
        }
        let child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| Launch::Fatal(format!("could not spawn {}: {err}", binary.display())))?;
        let mut server = Self {
            child,
            url: format!("nats://127.0.0.1:{port}"),
            access,
            port,
            _store: store,
        };
        for _ in 0..READY_ATTEMPTS {
            if let Ok(Some(status)) = server.child.try_wait() {
                return Err(Launch::Retry(format!(
                    "nats-server exited early on port {port} with {status}"
                )));
            }
            if let Ok(client) = access.connect(port, CALLER_USER).await {
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

    #[allow(dead_code)]
    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn client(&self) -> async_nats::Client {
        match self.access.connect(self.port, CALLER_USER).await {
            Ok(client) => client,
            Err(err) => panic!("connect to {}: {err}", self.url),
        }
    }

    /// A connection as the service's runtime user, which differs from [`Self::client`] only when
    /// the server restricts that user.
    #[allow(dead_code)]
    pub async fn runtime_client(&self) -> async_nats::Client {
        match self.access.connect(self.port, RUNTIME_USER).await {
            Ok(client) => client,
            Err(err) => panic!("connect as {RUNTIME_USER}: {err}"),
        }
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
