#![allow(dead_code)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use nkeys::{KeyPair, XKey};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use trogon_presence::{BucketName, ConnectionId, OperationId, PresenceConfig, Topic};
use trogon_presence_auth::{
    AccountName, AuthBucket, AuthCommands, AuthProvisionOptions, AuthRealmId, AuthStore, AuthorizationRequest, Callout,
    CalloutConcurrency, CalloutConfig, CalloutDeadline, CalloutXKey, ConnectIdentity, ConnectToken, ConnectionType,
    EnrollOutcome, EnrollRequest, GrantPolicy, IdentityBound, IssuerKey, KeyRing, PrivateTopicCap, RateLimit,
    ServerNkey, ServerRoster, ServerXKey, Subject, SweepConfig, SweepExecutor, TenantId, TenantMapping, TenantRegistry,
    TokenIssuer, TokenSigningKey, UnixSeconds, UserJwtLifetime, UserTarget, AUTH_CALLOUT_SUBJECT,
};
use trogon_presence_service::config::default_lease_bucket;
use trogon_presence_service::CALLER_INBOX_PREFIX;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type TestResult = Result<(), BoxError>;

const SERVER_BINARY_ENV: &str = "TROGON_PRESENCE_NATS_SERVER";
const START_ATTEMPTS: usize = 5;
const READY_ATTEMPTS: usize = 100;
const READY_BACKOFF: Duration = Duration::from_millis(50);
const AUTH_STREAM: &str = "KV_PRESENCE_AUTH_V1";
const AUTH_KV: &str = "$KV.PRESENCE_AUTH_V1";
pub const AUTH_ACCOUNT: &str = "AUTH";
pub const APP_A: &str = "APP_A";
pub const APP_B: &str = "APP_B";
pub const SYS_ACCOUNT: &str = "SYS";
pub const TENANT_A: &str = "alpha";
pub const TENANT_B: &str = "beta";
pub const REALM: &str = "test-realm";
pub const DEFAULT_SESSION: Duration = Duration::from_secs(3600);
pub const AUTH_TIMEOUT_SECS: u64 = 2;

#[derive(Clone)]
pub struct FixtureOptions {
    pub xkey: bool,
    pub policy: GrantPolicy,
    pub rate_limit: RateLimit,
    pub identity_bound: IdentityBound,
    pub concurrency: CalloutConcurrency,
    pub user_jwt_lifetime: UserJwtLifetime,
    pub websocket: bool,
    pub connection_types: Vec<ConnectionType>,
}

impl Default for FixtureOptions {
    fn default() -> Self {
        Self {
            xkey: true,
            policy: GrantPolicy::default(),
            rate_limit: RateLimit::default(),
            identity_bound: IdentityBound::default(),
            concurrency: CalloutConcurrency::default(),
            user_jwt_lifetime: UserJwtLifetime::default(),
            websocket: false,
            connection_types: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct Credential {
    pub user: String,
    password: String,
}

impl Credential {
    fn new(user: &str) -> Result<Self, BoxError> {
        Ok(Self {
            user: user.to_owned(),
            password: OperationId::generate()?.to_string(),
        })
    }

    fn entry(&self, publish: &[String], subscribe: &[String], responses: bool) -> Value {
        let mut permissions = json!({
            "publish": { "allow": publish },
            "subscribe": { "allow": subscribe },
        });
        if responses {
            permissions["allow_responses"] = json!(true);
        }
        json!({ "user": self.user, "password": self.password, "permissions": permissions })
    }

    pub async fn connect(&self, url: &str) -> Result<async_nats::Client, async_nats::ConnectError> {
        async_nats::ConnectOptions::with_user_and_password(self.user.clone(), self.password.clone())
            .connect(url)
            .await
    }

    pub async fn session(&self, url: &str) -> Result<Session, async_nats::ConnectError> {
        let (tx, events) = mpsc::unbounded_channel();
        let client = async_nats::ConnectOptions::with_user_and_password(self.user.clone(), self.password.clone())
            .event_callback(move |event| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(event.to_string());
                }
            })
            .connect(url)
            .await?;
        Ok(Session { client, events })
    }
}

pub struct TenantUsers {
    pub runtime: Credential,
    pub provisioner: Credential,
}

pub struct Users {
    pub issuer: Credential,
    pub command: Credential,
    pub callout: Credential,
    pub system: Credential,
    pub app_a: TenantUsers,
    pub app_b: TenantUsers,
}

impl Users {
    fn generate() -> Result<Self, BoxError> {
        let tenant = |suffix: &str| -> Result<TenantUsers, BoxError> {
            Ok(TenantUsers {
                runtime: Credential::new(&format!("runtime_{suffix}"))?,
                provisioner: Credential::new(&format!("provisioner_{suffix}"))?,
            })
        };
        Ok(Self {
            issuer: Credential::new("auth_issuer")?,
            command: Credential::new("auth_command")?,
            callout: Credential::new("auth_callout")?,
            system: Credential::new("sys_kick")?,
            app_a: tenant("a")?,
            app_b: tenant("b")?,
        })
    }

    fn names(&self) -> Vec<String> {
        [
            &self.issuer,
            &self.command,
            &self.callout,
            &self.system,
            &self.app_a.runtime,
            &self.app_a.provisioner,
            &self.app_b.runtime,
            &self.app_b.provisioner,
        ]
        .iter()
        .map(|credential| credential.user.clone())
        .collect()
    }
}

fn inbox() -> Vec<String> {
    vec!["_INBOX.>".to_owned()]
}

fn auth_rows(kinds: &[&str]) -> Vec<String> {
    kinds.iter().map(|kind| format!("{AUTH_KV}.{kind}.>")).collect()
}

fn auth_api(ops: &[&str]) -> Vec<String> {
    ops.iter()
        .map(|op| format!("$JS.API.STREAM.{op}.{AUTH_STREAM}"))
        .collect()
}

fn auth_rebuild() -> Vec<String> {
    vec![
        format!("$JS.API.CONSUMER.CREATE.{AUTH_STREAM}.*"),
        format!("$JS.API.CONSUMER.CREATE.{AUTH_STREAM}.*.>"),
        format!("$JS.API.CONSUMER.INFO.{AUTH_STREAM}.*"),
        format!("$JS.API.CONSUMER.MSG.NEXT.{AUTH_STREAM}.*"),
        format!("$JS.API.CONSUMER.DELETE.{AUTH_STREAM}.*"),
    ]
}

fn deny_all() -> Value {
    json!({ "publish": { "deny": [">"] }, "subscribe": { "deny": [">"] } })
}

fn runtime_stream_grants(bucket: &BucketName) -> Vec<String> {
    let stream = bucket.stream_name();
    vec![
        format!("$JS.API.STREAM.INFO.{stream}"),
        format!("$JS.API.STREAM.MSG.GET.{stream}"),
        format!("$JS.API.CONSUMER.CREATE.{stream}.*"),
        format!("$JS.API.CONSUMER.CREATE.{stream}.*.>"),
        format!("$JS.API.CONSUMER.INFO.{stream}.*"),
        format!("$JS.API.CONSUMER.MSG.NEXT.{stream}.*"),
        format!("$JS.API.CONSUMER.DELETE.{stream}.*"),
        format!("$JS.ACK.{stream}.>"),
        bucket.subjects_filter(),
    ]
}

fn provisioner_stream_grants(bucket: &BucketName) -> Vec<String> {
    let stream = bucket.stream_name();
    ["CREATE", "UPDATE", "INFO"]
        .iter()
        .map(|op| format!("$JS.API.STREAM.{op}.{stream}"))
        .collect()
}

fn tenant_account(users: &TenantUsers) -> Value {
    let presence = PresenceConfig::default().bucket().clone();
    let lease = default_lease_bucket();
    let runtime_publish = [
        runtime_stream_grants(&presence),
        runtime_stream_grants(&lease),
        vec!["presence.v1.>".to_owned(), format!("{CALLER_INBOX_PREFIX}.>")],
    ]
    .concat();
    let runtime_subscribe = vec!["_INBOX.>".to_owned(), "presence.v1.>".to_owned()];
    let probe = presence.probe_stream_name();
    let provisioner_publish = [
        vec!["$JS.API.INFO".to_owned()],
        provisioner_stream_grants(&presence),
        provisioner_stream_grants(&lease),
        ["CREATE", "INFO", "DELETE", "PURGE"]
            .iter()
            .map(|op| format!("$JS.API.STREAM.{op}.{probe}"))
            .collect(),
        vec![format!("{}.>", presence.probe_subject_root())],
    ]
    .concat();
    json!({
        "jetstream": "enabled",
        "default_permissions": deny_all(),
        "users": [
            users.runtime.entry(&runtime_publish, &runtime_subscribe, false),
            users.provisioner.entry(&provisioner_publish, &inbox(), false),
        ],
    })
}

fn server_config(
    port: u16,
    websocket: Option<u16>,
    store: &Path,
    users: &Users,
    issuer: &IssuerKey,
    xkey: Option<&CalloutXKey>,
) -> Value {
    let issuer_publish = [
        auth_api(&["CREATE", "INFO", "MSG.GET"]),
        auth_rows(&["policy", "session", "connection", "receipt", "sweep"]),
    ]
    .concat();
    let command_publish = [
        auth_api(&["INFO", "MSG.GET"]),
        auth_rows(&["policy", "session", "receipt", "sweep"]),
        auth_rebuild(),
    ]
    .concat();
    let callout_publish = [auth_api(&["INFO", "MSG.GET"]), auth_rows(&["policy", "grant"])].concat();
    let callout_subscribe = vec![AUTH_CALLOUT_SUBJECT.to_owned(), "_INBOX.>".to_owned()];
    let system_publish = vec![
        "$SYS.REQ.SERVER.*.CONNZ".to_owned(),
        "$SYS.REQ.SERVER.*.KICK".to_owned(),
    ];
    let mut callout = json!({
        "issuer": issuer.public_key(),
        "account": AUTH_ACCOUNT,
        "auth_users": users.names(),
    });
    if let Some(xkey) = xkey {
        callout["xkey"] = json!(xkey.public_key());
    }
    let mut config = json!({
        "listen": format!("127.0.0.1:{port}"),
        "jetstream": { "store_dir": store.display().to_string() },
        "system_account": SYS_ACCOUNT,
        "accounts": {
            AUTH_ACCOUNT: {
                "jetstream": "enabled",
                "default_permissions": deny_all(),
                "users": [
                    users.issuer.entry(&issuer_publish, &inbox(), false),
                    users.command.entry(&command_publish, &inbox(), false),
                    users.callout.entry(&callout_publish, &callout_subscribe, true),
                ],
            },
            APP_A: tenant_account(&users.app_a),
            APP_B: tenant_account(&users.app_b),
            SYS_ACCOUNT: {
                "default_permissions": deny_all(),
                "users": [users.system.entry(&system_publish, &inbox(), false)],
            },
        },
        "authorization": { "timeout": AUTH_TIMEOUT_SECS, "auth_callout": callout },
    });
    if let Some(websocket) = websocket {
        config["websocket"] = json!({ "listen": format!("127.0.0.1:{websocket}"), "no_tls": true });
    }
    config
}

fn free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

pub struct Fixture {
    child: Child,
    pub url: String,
    pub ws_url: Option<String>,
    /// The `client_info.type` direct requests report: websocket when the fixture listens for it.
    pub transport: &'static str,
    pub runtime_dir: TempDir,
    pub users: Users,
    pub issuer_store: AuthStore,
    pub callout_store: AuthStore,
    pub enroller: AuthCommands,
    pub commands: AuthCommands,
    pub callout: Callout,
    pub tokens: TokenIssuer,
    pub system: async_nats::Client,
    pub tenants: TenantRegistry,
    pub realm: AuthRealmId,
    serve: JoinHandle<()>,
}

pub struct Session {
    pub client: async_nats::Client,
    pub events: mpsc::UnboundedReceiver<String>,
}

pub struct DirectRequest {
    pub request: AuthorizationRequest,
    pub server_xkey: ServerXKey,
}

impl Fixture {
    pub async fn start(options: FixtureOptions) -> Option<Self> {
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
            match Self::launch(binary.path(), &options).await {
                Ok(fixture) => return Some(fixture),
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

    async fn launch(binary: &Path, options: &FixtureOptions) -> Result<Self, Launch> {
        macro_rules! fatal {
            ($result:expr, $what:literal) => {
                match $result {
                    Ok(value) => value,
                    Err(err) => return Err(Launch::Fatal(format!("{}: {err}", $what))),
                }
            };
        }
        let runtime_dir = fatal!(tempfile::tempdir(), "no runtime dir");
        let port = fatal!(free_port(), "no free port");
        let websocket = if options.websocket {
            Some(fatal!(free_port(), "no free websocket port"))
        } else {
            None
        };
        let users = fatal!(Users::generate(), "no credentials");
        let issuer = IssuerKey::generate();
        let xkey = CalloutXKey::generate();
        let config = server_config(
            port,
            websocket,
            &runtime_dir.path().join("js"),
            &users,
            &issuer,
            options.xkey.then_some(&xkey),
        );
        let config_path = runtime_dir.path().join("server.conf");
        let rendered = fatal!(serde_json::to_string_pretty(&config), "render config");
        fatal!(std::fs::write(&config_path, rendered), "write config");
        let checked = fatal!(
            Command::new(binary).arg("-c").arg(&config_path).arg("-t").output(),
            "run nats-server -t"
        );
        if !checked.status.success() {
            let reason = format!(
                "nats-server -t rejected the fixture config: {}",
                String::from_utf8_lossy(&checked.stderr).trim()
            );
            return Err(Launch::Fatal(reason));
        }
        let child = fatal!(
            Command::new(binary)
                .arg("-c")
                .arg(&config_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn(),
            "spawn nats-server"
        );
        let url = format!("nats://127.0.0.1:{port}");
        let mut child = Some(child);
        let mut ready = None;
        for _ in 0..READY_ATTEMPTS {
            if let Some(Ok(Some(status))) = child.as_mut().map(Child::try_wait) {
                let reason = format!("nats-server exited early on port {port} with {status}");
                return Err(Launch::Retry(reason));
            }
            if let Ok(client) = users.issuer.connect(&url).await {
                let provisioned =
                    AuthStore::provision(client.clone(), AuthBucket::default(), &AuthProvisionOptions::default()).await;
                if let Ok(store) = provisioned {
                    ready = Some(store);
                    break;
                }
            }
            tokio::time::sleep(READY_BACKOFF).await;
        }
        let mut child = match child {
            Some(child) => child,
            None => return Err(Launch::Fatal("no child".to_owned())),
        };
        let Some(issuer_store) = ready else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Launch::Retry(format!("nats-server did not become ready on {url}")));
        };
        Self::assemble(
            child,
            url,
            websocket.map(|port| format!("ws://127.0.0.1:{port}")),
            runtime_dir,
            users,
            issuer_store,
            issuer,
            xkey,
            options.clone(),
        )
        .await
        .map_err(Launch::Fatal)
    }

    #[allow(clippy::too_many_arguments)]
    async fn assemble(
        mut child: Child,
        url: String,
        ws_url: Option<String>,
        runtime_dir: TempDir,
        users: Users,
        issuer_store: AuthStore,
        issuer: IssuerKey,
        xkey: CalloutXKey,
        options: FixtureOptions,
    ) -> Result<Self, String> {
        let parts = async {
            let command_client = users.command.connect(&url).await?;
            let callout_client = users.callout.connect(&url).await?;
            let system = users.system.connect(&url).await?;
            let command_store = AuthStore::open(command_client, AuthBucket::default()).await?;
            let callout_store = AuthStore::open(callout_client.clone(), AuthBucket::default()).await?;
            let tokens = TokenIssuer::new("k1".parse()?, TokenSigningKey::generate()?);
            let previous = TokenIssuer::new("k0".parse()?, TokenSigningKey::generate()?);
            let keys = KeyRing::new([previous.key_ring_entry(), tokens.key_ring_entry()])?;
            let mappings: Vec<TenantMapping> = vec![
                format!("{TENANT_A}={APP_A}").parse()?,
                format!("{TENANT_B}={APP_B}").parse()?,
            ];
            let realm: AuthRealmId = REALM.parse()?;
            Ok::<_, BoxError>((
                command_store,
                callout_store,
                callout_client,
                system,
                tokens,
                keys,
                mappings,
                realm,
            ))
        }
        .await;
        let (command_store, callout_store, callout_client, system, tokens, keys, mappings, realm) = match parts {
            Ok(parts) => parts,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("fixture setup: {err}"));
            }
        };
        let registry = |mappings: Vec<TenantMapping>| TenantRegistry::new(mappings);
        let (tenants, callout_tenants) = match (registry(mappings.clone()), registry(mappings)) {
            (Ok(a), Ok(b)) => (a, b),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("tenant registry".to_owned());
            }
        };
        let cap = options.policy.max_private_topics;
        let config = CalloutConfig {
            issuer,
            issuer_account: None,
            xkey,
            keys,
            realm: realm.clone(),
            tenants: callout_tenants,
            policy: options.policy,
            user_jwt_lifetime: options.user_jwt_lifetime,
            rate_limit: options.rate_limit,
            identity_bound: options.identity_bound,
            concurrency: options.concurrency,
            deadline: CalloutDeadline::default(),
            allowed_connection_types: options.connection_types,
        };
        let callout = Callout::new(config, callout_store.clone());
        let serving = callout.clone();
        let serve = tokio::spawn(async move {
            let _ = serving.serve(callout_client).await;
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(Self {
            child,
            url,
            transport: if ws_url.is_some() { "websocket" } else { "nats" },
            ws_url,
            runtime_dir,
            users,
            enroller: AuthCommands::new(issuer_store.clone(), cap),
            issuer_store,
            callout_store,
            commands: AuthCommands::new(command_store, cap),
            callout,
            tokens,
            system,
            tenants,
            realm,
            serve,
        })
    }

    pub fn server(&self) -> Result<ServerNkey, BoxError> {
        Ok(self.system.server_info().server_id.parse()?)
    }

    pub fn sweep_config(&self, roster: Vec<ServerNkey>) -> Result<SweepConfig, BoxError> {
        Ok(SweepConfig::new(ServerRoster::new(roster)?, self.tenants.clone()))
    }

    pub async fn executor(&self, config: SweepConfig) -> Result<SweepExecutor, BoxError> {
        let system = self.users.system.connect(&self.url).await?;
        Ok(SweepExecutor::new(self.commands.store().clone(), system, config))
    }

    pub fn subject(raw: &str) -> Result<Subject, BoxError> {
        Ok(Subject::try_from(raw.to_owned())?)
    }

    pub fn user(tenant: &str, sub: &str) -> Result<UserTarget, BoxError> {
        Ok(UserTarget {
            operation: OperationId::generate()?,
            tenant: tenant.parse()?,
            sub: Self::subject(sub)?,
        })
    }

    pub fn enroll_request(
        &self,
        tenant: &str,
        sub: &str,
        sid: &str,
        topics: &[Topic],
        session: Duration,
    ) -> Result<EnrollRequest, BoxError> {
        Ok(EnrollRequest {
            user: Self::user(tenant, sub)?,
            realm: self.realm.clone(),
            sid: sid.parse()?,
            cid: ConnectionId::generate()?,
            topics: topics.to_vec(),
            session_expires_at: UnixSeconds::now().saturating_add(session),
        })
    }

    pub async fn enroll(&self, tenant: &str, sub: &str, topics: &[Topic]) -> Result<EnrollOutcome, BoxError> {
        let request = self.enroll_request(tenant, sub, "s1", topics, DEFAULT_SESSION)?;
        Ok(self.enroller.enroll(&request).await?)
    }

    pub fn account(&self, tenant: &TenantId) -> Result<AccountName, BoxError> {
        self.tenants
            .account(tenant)
            .cloned()
            .ok_or_else(|| "tenant is not registered".into())
    }

    pub fn token(&self, identity: &ConnectIdentity) -> Result<ConnectToken, BoxError> {
        let aud = self.account(&identity.tenant)?;
        self.token_for_account(identity, aud)
    }

    pub fn token_for_account(&self, identity: &ConnectIdentity, aud: AccountName) -> Result<ConnectToken, BoxError> {
        let claims = self.tokens.claims(identity, aud, UnixSeconds::now())?;
        Ok(self.tokens.mint(&claims)?)
    }

    pub async fn connect(
        &self,
        identity: &ConnectIdentity,
        token: &ConnectToken,
    ) -> Result<Session, async_nats::ConnectError> {
        self.connect_to(&self.url, identity, token).await
    }

    pub async fn connect_to(
        &self,
        url: &str,
        identity: &ConnectIdentity,
        token: &ConnectToken,
    ) -> Result<Session, async_nats::ConnectError> {
        let (tx, events) = mpsc::unbounded_channel();
        let prefix = format!("{CALLER_INBOX_PREFIX}.{}.{}", identity.sub.token(), identity.cid);
        let client = async_nats::ConnectOptions::with_token(token.as_str().to_owned())
            .custom_inbox_prefix(prefix)
            .event_callback(move |event| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(event.to_string());
                }
            })
            .connect(url)
            .await?;
        Ok(Session { client, events })
    }

    pub fn direct_request(&self, token: &ConnectToken) -> Result<DirectRequest, BoxError> {
        let server = KeyPair::new_server();
        let claims = json!({
            "aud": "nats-authorization-request",
            "iss": server.public_key(),
            "sub": KeyPair::new_user().public_key(),
            "iat": UnixSeconds::now().get(),
            "nats": {
                "type": "authorization_request",
                "server_id": { "id": server.public_key(), "name": "direct" },
                "user_nkey": KeyPair::new_user().public_key(),
                "client_info": { "id": 7, "host": "127.0.0.1", "kind": "Client", "type": self.transport },
                "connect_opts": { "auth_token": token.as_str() },
                "version": 2
            }
        });
        let header = json!({ "typ": "JWT", "alg": "ed25519-nkey" });
        let input = format!(
            "{}.{}",
            B64.encode(serde_json::to_vec(&header)?),
            B64.encode(serde_json::to_vec(&claims)?)
        );
        let signature = server.sign(input.as_bytes())?;
        let jwt = format!("{input}.{}", B64.encode(signature));
        Ok(DirectRequest {
            request: AuthorizationRequest::decode(&jwt)?,
            server_xkey: XKey::new().public_key().parse()?,
        })
    }
}

impl Session {
    async fn drain_violations(&mut self, subjects: &[String], kind: &str) -> Vec<String> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event.to_lowercase());
        }
        subjects
            .iter()
            .filter(|subject| {
                let quoted = format!("\"{}\"", subject.to_lowercase());
                events.iter().any(|event| {
                    event.contains("permissions violation") && event.contains(kind) && event.contains(&quoted)
                })
            })
            .cloned()
            .collect()
    }

    pub async fn subscribe_violations(&mut self, subjects: &[String]) -> Result<Vec<String>, BoxError> {
        let mut subscriptions = Vec::new();
        for subject in subjects {
            subscriptions.push(self.client.subscribe(subject.clone()).await?);
        }
        self.client.flush().await?;
        Ok(self.drain_violations(subjects, "subscription").await)
    }

    pub async fn publish_violations(&mut self, subjects: &[String]) -> Result<Vec<String>, BoxError> {
        for subject in subjects {
            self.client.publish(subject.clone(), "{}".into()).await?;
        }
        self.client.flush().await?;
        Ok(self.drain_violations(subjects, "publish").await)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.serve.abort();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn topics(raw: &[&str]) -> Result<Vec<Topic>, BoxError> {
    Ok(raw.iter().map(|t| t.parse()).collect::<Result<_, _>>()?)
}

pub fn private_cap(cap: usize) -> Result<PrivateTopicCap, BoxError> {
    Ok(PrivateTopicCap::new(cap)?)
}

pub fn jwt_claims(jwt: &str) -> Result<Value, BoxError> {
    let payload = jwt.split('.').nth(1).ok_or("jwt has no payload")?;
    Ok(serde_json::from_slice(&B64.decode(payload)?)?)
}

pub fn jwt_header(jwt: &str) -> Result<Value, BoxError> {
    let header = jwt.split('.').next().ok_or("jwt has no header")?;
    Ok(serde_json::from_slice(&B64.decode(header)?)?)
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
    fn path(&self) -> &Path {
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
