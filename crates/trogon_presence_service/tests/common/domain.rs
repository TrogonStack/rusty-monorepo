//! A hub and a leaf nats-server on loopback, each with its own JetStream domain, joined by one
//! leafnode connection in the default account.
//!
//! Clients pick a side with [`Side`]. JetStream API calls reach the other side's domain only through
//! its `$JS.<domain>.API` prefix, which is the deployment path under test.
#![allow(dead_code)]

use std::fmt;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use async_nats::jetstream;
use tempfile::TempDir;

const SERVER_BINARY_ENV: &str = "TROGON_PRESENCE_NATS_SERVER";
const FIXTURE_DIR_ENV: &str = "TROGON_PRESENCE_FIXTURE_DIR";
const START_ATTEMPTS: usize = 3;
const POLL: Duration = Duration::from_millis(100);
const READY: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub type DomainError = Box<dyn std::error::Error + Send + Sync>;

/// The two servers and the JetStream domain each one owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Hub,
    Leaf,
}

impl Side {
    pub const ALL: [Self; 2] = [Self::Hub, Self::Leaf];

    /// The JetStream domain this server serves.
    pub fn domain(self) -> &'static str {
        match self {
            Self::Hub => "hub",
            Self::Leaf => "leaf",
        }
    }

    /// The API prefix that reaches this side's JetStream from anywhere in the account.
    pub fn api_prefix(self) -> String {
        format!("$JS.{}.API", self.domain())
    }

    fn index(self) -> usize {
        match self {
            Self::Hub => 0,
            Self::Leaf => 1,
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.domain())
    }
}

/// The release a server reports in its INFO, for verdicts that differ between pinned releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerVersion(String);

impl ServerVersion {
    pub fn of(client: &async_nats::Client) -> Self {
        Self(client.server_info().version)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServerVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

struct Node {
    child: Option<Child>,
    client_port: u16,
}

impl Node {
    fn url(&self) -> String {
        format!("nats://127.0.0.1:{}", self.client_port)
    }

    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn exited(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))))
    }
}

pub struct DomainPair {
    nodes: Vec<Node>,
    _root: TempDir,
}

impl DomainPair {
    /// Starts the hub, then the leaf, and waits until a leaf client reaches the hub domain.
    /// Returns `None` when no server binary is available and none was pinned.
    pub async fn start() -> Option<Self> {
        let binary = resolve_binary()?;
        let mut last = String::new();
        for _ in 0..START_ATTEMPTS {
            match Self::launch(&binary).await {
                Ok(pair) => return Some(pair),
                Err(err) => last = err.to_string(),
            }
        }
        panic!("a hub and leaf from {} did not start: {last}", binary.display());
    }

    async fn launch(binary: &Path) -> Result<Self, DomainError> {
        let root = match std::env::var_os(FIXTURE_DIR_ENV) {
            Some(dir) => tempfile::tempdir_in(dir)?,
            None => tempfile::tempdir()?,
        };
        let ports = free_ports(3)?;
        let (hub_port, leaf_port, leafnode_port) = (ports[0], ports[1], ports[2]);
        let hub_config = format!(
            "server_name: hub\nlisten: \"127.0.0.1:{hub_port}\"\njetstream {{\n  store_dir: \"{store}\"\n  domain: {domain}\n}}\nleafnodes {{\n  listen: \"127.0.0.1:{leafnode_port}\"\n}}\n",
            store = store(root.path(), Side::Hub)?.display(),
            domain = Side::Hub.domain(),
        );
        let leaf_config = format!(
            "server_name: leaf\nlisten: \"127.0.0.1:{leaf_port}\"\njetstream {{\n  store_dir: \"{store}\"\n  domain: {domain}\n}}\nleafnodes {{\n  reconnect: \"1s\"\n  remotes: [{{ url: \"nats-leaf://127.0.0.1:{leafnode_port}\" }}]\n}}\n",
            store = store(root.path(), Side::Leaf)?.display(),
            domain = Side::Leaf.domain(),
        );
        let mut pair = Self {
            nodes: Vec::with_capacity(2),
            _root: root,
        };
        for (side, port, config) in [(Side::Hub, hub_port, hub_config), (Side::Leaf, leaf_port, leaf_config)] {
            let path = pair._root.path().join(format!("{side}.conf"));
            std::fs::write(&path, config)?;
            let child = Command::new(binary)
                .arg("-c")
                .arg(&path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|err| format!("could not spawn {}: {err}", binary.display()))?;
            pair.nodes.push(Node {
                child: Some(child),
                client_port: port,
            });
        }
        pair.wait_ready().await?;
        Ok(pair)
    }

    async fn wait_ready(&mut self) -> Result<(), DomainError> {
        let started = Instant::now();
        while started.elapsed() < READY {
            for side in Side::ALL {
                if self.nodes[side.index()].exited() {
                    return Err(format!("{side} exited before it became ready").into());
                }
            }
            if self.leaf_reaches_every_domain().await {
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
        }
        Err(format!("the leaf did not reach both JetStream domains within {READY:?}").into())
    }

    async fn leaf_reaches_every_domain(&self) -> bool {
        let Ok(client) = self.client(Side::Leaf).await else {
            return false;
        };
        for side in Side::ALL {
            if jetstream::with_domain(client.clone(), side.domain())
                .query_account()
                .await
                .is_err()
            {
                return false;
            }
        }
        true
    }

    pub fn url(&self, side: Side) -> String {
        self.nodes[side.index()].url()
    }

    /// A client connected to one server only.
    pub async fn client(&self, side: Side) -> Result<async_nats::Client, DomainError> {
        Ok(async_nats::ConnectOptions::new()
            .ignore_discovered_servers()
            .connection_timeout(CONNECT_TIMEOUT)
            .request_timeout(Some(REQUEST_TIMEOUT))
            .connect(self.url(side))
            .await?)
    }

    /// A JetStream context on `client` that addresses the JetStream domain of `target`.
    pub fn context(client: &async_nats::Client, target: Side) -> jetstream::Context {
        jetstream::with_domain(client.clone(), target.domain())
    }
}

impl Drop for DomainPair {
    fn drop(&mut self) {
        for node in self.nodes.iter_mut().rev() {
            node.kill();
        }
    }
}

fn store(root: &Path, side: Side) -> std::io::Result<PathBuf> {
    let path = root.join(side.domain());
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn free_ports(count: usize) -> std::io::Result<Vec<u16>> {
    let listeners = (0..count)
        .map(|_| TcpListener::bind("127.0.0.1:0"))
        .collect::<std::io::Result<Vec<_>>>()?;
    listeners
        .iter()
        .map(|listener| listener.local_addr().map(|addr| addr.port()))
        .collect()
}

fn resolve_binary() -> Option<PathBuf> {
    if let Some(pinned) = std::env::var_os(SERVER_BINARY_ENV) {
        return Some(PathBuf::from(pinned));
    }
    let on_path = Command::new("nats-server")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if on_path {
        return Some(PathBuf::from("nats-server"));
    }
    let found = Command::new("mise")
        .args(["which", "nats-server"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|path| PathBuf::from(path.trim()));
    if found.is_none() {
        eprintln!("skipping: nats-server is neither on PATH nor resolvable through `mise which nats-server`");
    }
    found
}
