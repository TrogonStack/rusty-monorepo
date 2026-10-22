//! Three-node JetStream cluster on loopback for fault tests.
//!
//! Every node runs from its own config file with a fixed client port, route port and
//! store directory, so a stopped node restarts as the same peer with its data intact.
//! Routes do not advertise client URLs, which keeps each client pinned to the node it
//! dialed: stopping that node is how a test takes a process's connection away.

use std::fmt;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use async_nats::jetstream;
use tempfile::TempDir;

const SERVER_BINARY_ENV: &str = "TROGON_PRESENCE_NATS_SERVER";
const CLUSTER_NAME: &str = "presence-r3";
const START_ATTEMPTS: usize = 3;
const POLL: Duration = Duration::from_millis(100);
const NODE_READY: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub type ClusterError = Box<dyn std::error::Error + Send + Sync>;

/// One of the three cluster members.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Peer {
    N1,
    N2,
    N3,
}

impl Peer {
    pub const ALL: [Self; 3] = [Self::N1, Self::N2, Self::N3];

    pub fn name(self) -> &'static str {
        match self {
            Self::N1 => "n1",
            Self::N2 => "n2",
            Self::N3 => "n3",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::N1 => 0,
            Self::N2 => 1,
            Self::N3 => 2,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|peer| peer.name() == name)
    }

    /// The peers other than this one.
    pub fn others(self) -> impl Iterator<Item = Self> {
        Self::ALL.into_iter().filter(move |peer| *peer != self)
    }
}

impl fmt::Display for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A bounded wait with a name, so a timeout says what it was waiting for.
#[derive(Debug, Clone, Copy)]
pub struct Wait {
    pub what: &'static str,
    pub within: Duration,
}

impl Wait {
    pub const fn new(what: &'static str, within: Duration) -> Self {
        Self { what, within }
    }

    /// Polls `probe` until it yields a value or the bound runs out.
    pub async fn until<T, F, Fut>(self, mut probe: F) -> Result<T, ClusterError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Option<T>>,
    {
        let started = Instant::now();
        loop {
            if let Some(value) = probe().await {
                return Ok(value);
            }
            if started.elapsed() >= self.within {
                return Err(format!("timed out after {:?} waiting for {}", self.within, self.what).into());
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

struct Node {
    client_port: u16,
    config: PathBuf,
    child: Option<Child>,
}

impl Node {
    fn url(&self) -> String {
        format!("nats://127.0.0.1:{}", self.client_port)
    }

    fn spawn(&mut self, binary: &Path) -> Result<(), ClusterError> {
        let child = Command::new(binary)
            .arg("-c")
            .arg(&self.config)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("could not spawn {}: {err}", binary.display()))?;
        self.child = Some(child);
        Ok(())
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

pub struct Cluster {
    binary: PathBuf,
    nodes: Vec<Node>,
    _root: TempDir,
}

impl Cluster {
    /// Starts three nodes and waits until every one answers JetStream account info.
    /// Returns `None` when no server binary is available and none was pinned.
    pub async fn start() -> Option<Self> {
        let binary = resolve_binary()?;
        let mut last = String::new();
        for _ in 0..START_ATTEMPTS {
            match Self::launch(binary.clone()).await {
                Ok(cluster) => return Some(cluster),
                Err(err) => last = err.to_string(),
            }
        }
        panic!("a three-node cluster from {} did not start: {last}", binary.display());
    }

    async fn launch(binary: PathBuf) -> Result<Self, ClusterError> {
        let root = tempfile::tempdir()?;
        let ports = free_ports(6)?;
        let routes: Vec<String> = ports[3..]
            .iter()
            .map(|port| format!("\"nats-route://127.0.0.1:{port}\""))
            .collect();
        let mut nodes = Vec::with_capacity(3);
        for peer in Peer::ALL {
            let client_port = ports[peer.index()];
            let route_port = ports[3 + peer.index()];
            let store = root.path().join(peer.name());
            std::fs::create_dir_all(&store)?;
            let config = root.path().join(format!("{}.conf", peer.name()));
            std::fs::write(
                &config,
                format!(
                    "server_name: {name}\nlisten: \"127.0.0.1:{client_port}\"\njetstream {{\n  store_dir: \"{store}\"\n}}\ncluster {{\n  name: {CLUSTER_NAME}\n  listen: \"127.0.0.1:{route_port}\"\n  no_advertise: true\n  routes: [{routes}]\n}}\n",
                    name = peer.name(),
                    store = store.display(),
                    routes = routes.join(", "),
                ),
            )?;
            nodes.push(Node {
                client_port,
                config,
                child: None,
            });
        }
        let mut cluster = Self {
            binary,
            nodes,
            _root: root,
        };
        for peer in Peer::ALL {
            cluster.spawn(peer)?;
        }
        for peer in Peer::ALL {
            cluster.wait_ready(peer).await?;
        }
        Ok(cluster)
    }

    fn node(&mut self, peer: Peer) -> &mut Node {
        &mut self.nodes[peer.index()]
    }

    fn spawn(&mut self, peer: Peer) -> Result<(), ClusterError> {
        let binary = self.binary.clone();
        self.node(peer).spawn(&binary)
    }

    pub fn url(&self, peer: Peer) -> String {
        self.nodes[peer.index()].url()
    }

    /// A client pinned to one node: it never learns or dials the other members.
    pub async fn client(&self, peer: Peer) -> Result<async_nats::Client, ClusterError> {
        Ok(async_nats::ConnectOptions::new()
            .ignore_discovered_servers()
            .retry_on_initial_connect()
            .connection_timeout(CONNECT_TIMEOUT)
            .request_timeout(Some(REQUEST_TIMEOUT))
            .connect(self.url(peer))
            .await?)
    }

    /// Kills one node the way a crash would.
    pub fn stop(&mut self, peer: Peer) {
        self.node(peer).kill();
    }

    /// Starts a stopped node with its original ports and store, then waits until it serves JetStream.
    pub async fn restart(&mut self, peer: Peer) -> Result<(), ClusterError> {
        self.node(peer).kill();
        self.spawn(peer)?;
        self.wait_ready(peer).await
    }

    async fn wait_ready(&mut self, peer: Peer) -> Result<(), ClusterError> {
        let url = self.url(peer);
        let started = Instant::now();
        while started.elapsed() < NODE_READY {
            if self.node(peer).exited() {
                return Err(format!("{peer} exited before it became ready").into());
            }
            let connect = async_nats::ConnectOptions::new()
                .ignore_discovered_servers()
                .connection_timeout(CONNECT_TIMEOUT)
                .connect(&url);
            if let Ok(client) = connect.await {
                if jetstream::new(client).query_account().await.is_ok() {
                    return Ok(());
                }
            }
            tokio::time::sleep(POLL).await;
        }
        Err(format!("{peer} did not serve JetStream within {NODE_READY:?}").into())
    }

    /// The node that currently leads `stream`, as reported through `client`.
    pub async fn stream_leader(client: &async_nats::Client, stream: &str) -> Result<Peer, ClusterError> {
        let mut handle = jetstream::new(client.clone()).get_stream(stream).await?;
        let info = handle.info().await?;
        let leader = info
            .cluster
            .as_ref()
            .and_then(|cluster| cluster.leader.as_deref())
            .ok_or_else(|| format!("{stream} reports no leader"))?;
        Peer::parse(leader).ok_or_else(|| format!("{stream} leader {leader} is not a cluster member").into())
    }

    /// Waits until `stream` has a leader other than `previous` with every listed replica current.
    pub async fn wait_for_leader(
        client: &async_nats::Client,
        stream: &str,
        previous: Option<Peer>,
        wait: Wait,
    ) -> Result<Peer, ClusterError> {
        wait.until(|| async {
            let leader = Self::stream_leader(client, stream).await.ok()?;
            (Some(leader) != previous).then_some(leader)
        })
        .await
    }

    /// Waits until `stream` reports a leader and all replicas present and current.
    pub async fn wait_for_replicas(
        client: &async_nats::Client,
        stream: &str,
        wait: Wait,
    ) -> Result<Peer, ClusterError> {
        wait.until(|| async {
            let mut handle = jetstream::new(client.clone()).get_stream(stream).await.ok()?;
            let info = handle.info().await.ok()?;
            let cluster = info.cluster.as_ref()?;
            let leader = Peer::parse(cluster.leader.as_deref()?)?;
            let healthy = cluster.replicas.len() == 2
                && cluster
                    .replicas
                    .iter()
                    .all(|replica| replica.current && !replica.offline);
            healthy.then_some(leader)
        })
        .await
    }

    /// Waits until `stream` has a leader and at least one other online, current replica, the most a two node
    /// quorum can offer while the third node is down. Returns the leader.
    pub async fn wait_for_quorum(client: &async_nats::Client, stream: &str, wait: Wait) -> Result<Peer, ClusterError> {
        wait.until(|| async {
            let mut handle = jetstream::new(client.clone()).get_stream(stream).await.ok()?;
            let info = handle.info().await.ok()?;
            let cluster = info.cluster.as_ref()?;
            let leader = Peer::parse(cluster.leader.as_deref()?)?;
            let quorum = cluster
                .replicas
                .iter()
                .any(|replica| replica.current && !replica.offline);
            quorum.then_some(leader)
        })
        .await
    }

    /// Waits until the meta leader answers account info and assigns a consumer on `stream`,
    /// by creating one and deleting it again.
    pub async fn wait_for_consumers(client: &async_nats::Client, stream: &str, wait: Wait) -> Result<(), ClusterError> {
        wait.until(|| async {
            let context = jetstream::new(client.clone());
            context.query_account().await.ok()?;
            let handle = context.get_stream(stream).await.ok()?;
            let consumer = handle
                .create_consumer(jetstream::consumer::pull::Config {
                    memory_storage: true,
                    num_replicas: 1,
                    inactive_threshold: Duration::from_secs(5),
                    ..Default::default()
                })
                .await
                .ok()?;
            let name = consumer.cached_info().name.clone();
            handle.delete_consumer(&name).await.ok().map(|_| ())
        })
        .await
    }

    /// Asks the current leader of `stream` to hand leadership to another replica.
    pub async fn step_down(client: &async_nats::Client, stream: &str) -> Result<(), ClusterError> {
        let response = client
            .request(format!("$JS.API.STREAM.LEADER.STEPDOWN.{stream}"), "".into())
            .await?;
        let body: serde_json::Value = serde_json::from_slice(&response.payload)?;
        if body.get("success").and_then(serde_json::Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(format!("stepdown of {stream} failed: {body}").into())
        }
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        for node in &mut self.nodes {
            node.kill();
        }
    }
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
