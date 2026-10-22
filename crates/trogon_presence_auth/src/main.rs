use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use trogon_presence::{ConnectionId, OperationId, ShardCount};
use trogon_presence_auth::{
    revoke, AccountName, AuthBucket, AuthCommands, AuthProvisionOptions, AuthRealmId, AuthSessionId, AuthStore,
    Callout, CalloutConcurrency, CalloutConfig, CalloutDeadline, CalloutXKey, ConnectionType, ConnzPageLimit,
    GrantPolicy, IdentityBound, IssuerAccount, IssuerKey, KeyRing, KeyRingEntry, PrivateTopicCap, PublicPrefix,
    RateLimit, RefreshRequest, ResponseBudget, ServerNkey, ServerRoster, SessionCeiling, SessionTarget, Subject,
    SweepConfig, SweepDeadline, SweepExecutor, SweepState, TenantId, TenantMapping, TenantRegistry, UnixSeconds,
    UserJwtLifetime, UserTarget, DEFAULT_CONNZ_PAGE_LIMIT, DEFAULT_MAX_PRIVATE_TOPICS, DEFAULT_RESPONSE_BUDGET_BYTES,
    DEFAULT_SESSION_CEILING, DEFAULT_SWEEP_DEADLINE,
};

#[derive(Debug, Parser)]
#[command(
    name = "trogon-presence-auth",
    version,
    about = "NATS auth callout for trogon presence"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run(RunArgs),
    Revoke(RevokeArgs),
    Refresh(RefreshArgs),
}

#[derive(Debug, Args)]
struct ConnectArgs {
    #[arg(long, env = "TROGON_PRESENCE_AUTH_NATS_URL", default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
    #[arg(long, env = "TROGON_PRESENCE_AUTH_NATS_CREDS", conflicts_with = "nats_user")]
    nats_creds: Option<PathBuf>,
    #[arg(long, env = "TROGON_PRESENCE_AUTH_NATS_USER", requires = "nats_password")]
    nats_user: Option<String>,
    #[arg(long, env = "TROGON_PRESENCE_AUTH_NATS_PASSWORD", hide_env_values = true)]
    nats_password: Option<String>,
    #[arg(long, env = "TROGON_PRESENCE_AUTH_BUCKET", default_value_t = AuthBucket::default())]
    bucket: AuthBucket,
}

#[derive(Debug, Args)]
struct SystemArgs {
    #[arg(long, env = "TROGON_PRESENCE_AUTH_SYSTEM_CREDS", conflicts_with = "system_user")]
    system_creds: Option<PathBuf>,
    #[arg(long, env = "TROGON_PRESENCE_AUTH_SYSTEM_USER", requires = "system_password")]
    system_user: Option<String>,
    #[arg(long, env = "TROGON_PRESENCE_AUTH_SYSTEM_PASSWORD", hide_env_values = true)]
    system_password: Option<String>,
    #[arg(long = "server", env = "TROGON_PRESENCE_AUTH_SERVERS", value_delimiter = ',')]
    servers: Vec<ServerNkey>,
    #[arg(long, default_value_t = DEFAULT_CONNZ_PAGE_LIMIT)]
    connz_page_limit: usize,
}

impl SystemArgs {
    async fn executor(
        &self,
        url: &str,
        store: AuthStore,
        tenants: TenantRegistry,
        lifetime: UserJwtLifetime,
    ) -> Result<SweepExecutor, BoxError> {
        let system = connect(
            url,
            self.system_creds.as_ref(),
            self.system_user.as_ref(),
            self.system_password.as_ref(),
        )
        .await?;
        let mut config = SweepConfig::new(ServerRoster::new(self.servers.clone())?, tenants);
        config.page_limit = ConnzPageLimit::try_from(self.connz_page_limit)?;
        config.user_jwt_lifetime = lifetime;
        Ok(SweepExecutor::new(store, system, config))
    }
}

#[derive(Debug, Args)]
struct RunArgs {
    #[command(flatten)]
    connect: ConnectArgs,
    #[command(flatten)]
    system: SystemArgs,
    #[arg(long, env = "TROGON_PRESENCE_AUTH_ISSUER_SEED", hide_env_values = true)]
    issuer_seed: String,
    #[arg(long, env = "TROGON_PRESENCE_AUTH_ISSUER_ACCOUNT")]
    issuer_account: Option<IssuerAccount>,
    #[arg(long, env = "TROGON_PRESENCE_AUTH_XKEY_SEED", hide_env_values = true)]
    xkey_seed: String,
    #[arg(long, env = "TROGON_PRESENCE_AUTH_REALM")]
    realm: AuthRealmId,
    #[arg(
        long = "token-key",
        env = "TROGON_PRESENCE_AUTH_TOKEN_KEYS",
        value_delimiter = ',',
        required = true
    )]
    token_keys: Vec<KeyRingEntry>,
    #[arg(
        long = "tenant",
        env = "TROGON_PRESENCE_AUTH_TENANTS",
        value_delimiter = ',',
        required = true
    )]
    tenants: Vec<TenantMapping>,
    #[arg(
        long = "public-prefix",
        env = "TROGON_PRESENCE_AUTH_PUBLIC_PREFIXES",
        value_delimiter = ','
    )]
    public_prefixes: Vec<PublicPrefix>,
    #[arg(long, default_value_t = DEFAULT_MAX_PRIVATE_TOPICS)]
    max_private_topics: usize,
    #[arg(long, default_value_t = DEFAULT_RESPONSE_BUDGET_BYTES)]
    response_budget_bytes: usize,
    #[arg(long, default_value_t = 600)]
    user_jwt_lifetime_secs: u64,
    #[arg(long, default_value_t = 900)]
    user_jwt_ceiling_secs: u64,
    #[arg(long = "connection-type", value_delimiter = ',')]
    connection_types: Vec<ConnectionType>,
    #[arg(long)]
    provision: bool,
}

#[derive(Debug, Args)]
struct RevokeArgs {
    #[command(flatten)]
    connect: ConnectArgs,
    #[command(flatten)]
    system: SystemArgs,
    #[arg(long)]
    tenant: TenantId,
    #[arg(long)]
    account: AccountName,
    #[arg(long)]
    sub: String,
    #[arg(long, default_value_t = DEFAULT_SWEEP_DEADLINE.as_secs())]
    sweep_deadline_secs: u64,
}

#[derive(Debug, Args)]
struct RefreshArgs {
    #[command(flatten)]
    connect: ConnectArgs,
    #[arg(long, env = "TROGON_PRESENCE_AUTH_REALM")]
    realm: AuthRealmId,
    #[arg(long)]
    tenant: TenantId,
    #[arg(long)]
    sub: String,
    #[arg(long)]
    sid: AuthSessionId,
    #[arg(long)]
    cid: ConnectionId,
    #[arg(long)]
    operation: Option<OperationId>,
    #[arg(long)]
    session_secs: u64,
    #[arg(long, default_value_t = DEFAULT_SESSION_CEILING.as_secs())]
    session_ceiling_secs: u64,
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn connect(
    url: &str,
    creds: Option<&PathBuf>,
    user: Option<&String>,
    password: Option<&String>,
) -> Result<async_nats::Client, BoxError> {
    let options = match (creds, user, password) {
        (Some(path), _, _) => async_nats::ConnectOptions::with_credentials_file(path).await?,
        (None, Some(user), Some(password)) => {
            async_nats::ConnectOptions::with_user_and_password(user.clone(), password.clone())
        }
        _ => async_nats::ConnectOptions::new(),
    };
    Ok(options.name("trogon-presence-auth").connect(url).await?)
}

async fn run(args: RunArgs) -> Result<(), BoxError> {
    let c = &args.connect;
    let client = connect(
        &c.nats_url,
        c.nats_creds.as_ref(),
        c.nats_user.as_ref(),
        c.nats_password.as_ref(),
    )
    .await?;
    let store = if args.provision {
        AuthStore::provision(client.clone(), c.bucket.clone(), &AuthProvisionOptions::default()).await?
    } else {
        AuthStore::open(client.clone(), c.bucket.clone()).await?
    };
    let tenants = args.tenants;
    let lifetime = UserJwtLifetime::new(
        Duration::from_secs(args.user_jwt_lifetime_secs),
        Duration::from_secs(args.user_jwt_ceiling_secs),
    )?;
    let config = CalloutConfig {
        issuer: args.issuer_seed.parse::<IssuerKey>()?,
        issuer_account: args.issuer_account,
        xkey: args.xkey_seed.parse::<CalloutXKey>()?,
        keys: KeyRing::new(args.token_keys)?,
        realm: args.realm,
        tenants: TenantRegistry::new(tenants.clone())?,
        policy: GrantPolicy {
            public_prefixes: args.public_prefixes,
            max_private_topics: PrivateTopicCap::new(args.max_private_topics)?,
            response_budget: ResponseBudget::bytes(args.response_budget_bytes)?,
            shards: ShardCount::default(),
        },
        user_jwt_lifetime: lifetime,
        rate_limit: RateLimit::default(),
        identity_bound: IdentityBound::default(),
        concurrency: CalloutConcurrency::default(),
        deadline: CalloutDeadline::default(),
        allowed_connection_types: args.connection_types,
    };
    let callout = Callout::new(config, store.clone());
    let executor = args
        .system
        .executor(&c.nats_url, store, TenantRegistry::new(tenants)?, lifetime)
        .await?;
    tracing::info!("serving auth callout and resuming revocation sweeps");
    tokio::select! {
        served = callout.serve(client) => served?,
        () = executor.serve() => {}
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
    }
    Ok(())
}

async fn revoke_command(args: RevokeArgs) -> Result<(), BoxError> {
    let c = &args.connect;
    let client = connect(
        &c.nats_url,
        c.nats_creds.as_ref(),
        c.nats_user.as_ref(),
        c.nats_password.as_ref(),
    )
    .await?;
    let store = AuthStore::open(client, c.bucket.clone()).await?;
    let tenants = TenantRegistry::new([TenantMapping {
        tenant: args.tenant.clone(),
        account: args.account,
    }])?;
    let executor = args
        .system
        .executor(&c.nats_url, store.clone(), tenants, UserJwtLifetime::default())
        .await?;
    let commands = AuthCommands::new(store, PrivateTopicCap::default());
    let target = UserTarget {
        operation: OperationId::generate()?,
        tenant: args.tenant,
        sub: Subject::try_from(args.sub)?,
    };
    let deadline = SweepDeadline::try_from(Duration::from_secs(args.sweep_deadline_secs))?;
    let report = revoke(&commands, &executor, &target, deadline).await?;
    println!(
        "state_committed={} epoch={} operation={}",
        report.state_committed(),
        report.outcome.receipt.epoch,
        target.operation
    );
    let Some(sweep) = report.sweep else {
        println!("sweep=not_required");
        return Ok(());
    };
    println!("sweep={:?} kicked={}", sweep.state, sweep.kicked.len());
    for failure in &sweep.failed {
        eprintln!(
            "kick failed server={} cid={}: {}",
            failure.connection.server, failure.connection.client, failure.reason
        );
    }
    match sweep.state {
        SweepState::Complete { .. } => Ok(()),
        SweepState::Pending | SweepState::InProgress(_) | SweepState::Incomplete(_) => {
            Err("sweep is not complete; the auth process resumes it from the stored sweep row".into())
        }
    }
}

async fn refresh_command(args: RefreshArgs) -> Result<(), BoxError> {
    let c = &args.connect;
    let client = connect(
        &c.nats_url,
        c.nats_creds.as_ref(),
        c.nats_user.as_ref(),
        c.nats_password.as_ref(),
    )
    .await?;
    let store = AuthStore::open(client, c.bucket.clone()).await?;
    let ceiling = SessionCeiling::new(Duration::from_secs(args.session_ceiling_secs))?;
    let commands = AuthCommands::new(store, PrivateTopicCap::default()).with_session_ceiling(ceiling);
    let operation = match args.operation {
        Some(operation) => operation,
        None => OperationId::generate()?,
    };
    let request = RefreshRequest {
        session: SessionTarget {
            user: UserTarget {
                operation,
                tenant: args.tenant,
                sub: Subject::try_from(args.sub)?,
            },
            sid: args.sid,
        },
        realm: args.realm,
        cid: args.cid,
        session_expires_at: UnixSeconds::now().saturating_add(Duration::from_secs(args.session_secs)),
    };
    let refreshed = commands.refresh_session(&request).await?;
    println!(
        "state={:?} asv={} epoch={} session_expires_at={} operation={operation}",
        refreshed.outcome.state,
        refreshed.identity.asv,
        refreshed.identity.auth_epoch,
        refreshed.identity.session_expires_at,
    );
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Run(args) => run(args).await,
        Command::Revoke(args) => revoke_command(args).await,
        Command::Refresh(args) => refresh_command(args).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
