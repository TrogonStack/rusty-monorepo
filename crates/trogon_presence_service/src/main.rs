use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use async_nats::jetstream::stream::StorageType;
use clap::{Args, Parser, Subcommand, ValueEnum};
use trogon_presence::{
    BucketName, CoalesceWindow, HeartbeatInterval, InflightBatchLimit, JetStreamDomain, LeaseTtl, MarkerTtl,
    PresenceConfig, ProvisionOptions, Replicas, ShardCount,
};
use trogon_presence_hooks::{AllowedHost, HookConfig, HookDeadline, HookMemoryLimit, HookPolicy};
use trogon_presence_service::config::default_lease_bucket;
use trogon_presence_service::{KeepaliveInterval, NodeId, PayloadBudget, ServiceConfig, ShardLeaseTtl, SnapshotLimits};

#[derive(Debug, Parser)]
#[command(
    name = "trogon-presence",
    about = "Presence diff broadcaster and request service on NATS"
)]
struct Cli {
    #[command(flatten)]
    options: Options,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Provision,
    Run(RunArgs),
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(long, env = "TROGON_PRESENCE_HOOK_COMPONENT")]
    hook_component: Option<PathBuf>,
    #[arg(long, env = "TROGON_PRESENCE_HOOK_POLICY", default_value_t = HookPolicy::default(), requires = "hook_component")]
    hook_policy: HookPolicy,
    #[arg(long, env = "TROGON_PRESENCE_HOOK_DEADLINE", value_parser = parse_duration, requires = "hook_component")]
    hook_deadline: Option<Duration>,
    #[arg(long, env = "TROGON_PRESENCE_HOOK_MEMORY", requires = "hook_component")]
    hook_memory: Option<HookMemoryLimit>,
    #[arg(
        long,
        env = "TROGON_PRESENCE_HOOK_HTTP_ALLOW",
        value_delimiter = ',',
        num_args = 1..,
        requires = "hook_component"
    )]
    hook_http_allow: Vec<AllowedHost>,
}

impl RunArgs {
    fn hook(&self) -> Result<Option<HookConfig>, BoxError> {
        let Some(path) = &self.hook_component else {
            return Ok(None);
        };
        let mut hook = HookConfig::new(path);
        hook.policy = self.hook_policy;
        hook.http_allow = self.hook_http_allow.clone();
        if let Some(deadline) = self.hook_deadline {
            hook.deadline = HookDeadline::try_from(deadline)?;
        }
        if let Some(memory) = self.hook_memory {
            hook.memory_limit = memory;
        }
        Ok(Some(hook))
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Storage {
    File,
    Memory,
}

impl From<Storage> for StorageType {
    fn from(storage: Storage) -> Self {
        match storage {
            Storage::File => Self::File,
            Storage::Memory => Self::Memory,
        }
    }
}

#[derive(Debug, Args)]
struct Options {
    #[arg(
        long,
        global = true,
        env = "TROGON_PRESENCE_NATS_URL",
        default_value = "nats://127.0.0.1:4222"
    )]
    nats_url: String,
    #[arg(long, global = true, env = "TROGON_PRESENCE_NATS_CREDENTIALS")]
    nats_credentials: Option<PathBuf>,
    #[arg(long, global = true, env = "TROGON_PRESENCE_BUCKET", default_value_t = BucketName::default())]
    bucket: BucketName,
    #[arg(long, global = true, env = "TROGON_PRESENCE_JS_DOMAIN")]
    jetstream_domain: Option<JetStreamDomain>,
    #[arg(long, global = true, env = "TROGON_PRESENCE_LEASE_BUCKET", default_value_t = default_lease_bucket())]
    lease_bucket: BucketName,
    #[arg(long, global = true, env = "TROGON_PRESENCE_SHARDS", default_value_t = ShardCount::DEFAULT.get())]
    shards: u16,
    #[arg(long, global = true, env = "TROGON_PRESENCE_LEASE_TTL", value_parser = parse_duration)]
    lease_ttl: Option<Duration>,
    #[arg(long, global = true, env = "TROGON_PRESENCE_MARKER_TTL", value_parser = parse_duration)]
    marker_ttl: Option<Duration>,
    #[arg(long, global = true, env = "TROGON_PRESENCE_HEARTBEAT_INTERVAL", value_parser = parse_duration)]
    heartbeat_interval: Option<Duration>,
    #[arg(long, global = true, env = "TROGON_PRESENCE_SHARD_LEASE_TTL", value_parser = parse_duration)]
    shard_lease_ttl: Option<Duration>,
    #[arg(long, global = true, env = "TROGON_PRESENCE_REPLICAS", default_value_t = 1)]
    replicas: u8,
    #[arg(
        long,
        global = true,
        env = "TROGON_PRESENCE_STORAGE",
        value_enum,
        default_value = "file"
    )]
    storage: Storage,
    #[arg(long, global = true, env = "TROGON_PRESENCE_KEEPALIVE_INTERVAL", value_parser = parse_duration)]
    keepalive_interval: Option<Duration>,
    #[arg(long, global = true, env = "TROGON_PRESENCE_COALESCE_WINDOW", value_parser = parse_duration)]
    coalesce_window: Option<Duration>,
    #[arg(long, global = true, env = "TROGON_PRESENCE_PAYLOAD_BUDGET_BYTES")]
    payload_budget_bytes: Option<usize>,
    #[arg(long, global = true, env = "TROGON_PRESENCE_NODE_ID")]
    node_id: Option<NodeId>,
    #[arg(long, global = true, env = "TROGON_PRESENCE_INFLIGHT_BATCHES", default_value_t = InflightBatchLimit::default())]
    inflight_batches: InflightBatchLimit,
    #[arg(long, global = true, env = "TROGON_PRESENCE_LOG_LEVEL", default_value = "info")]
    log_level: String,
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn parse_duration(raw: &str) -> Result<Duration, String> {
    let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (digits, unit) = raw.split_at(split);
    let value: u64 = digits
        .parse()
        .map_err(|_| format!("{raw:?} is not a duration like 30s, 100ms or 1m"))?;
    match unit {
        "ms" => Ok(Duration::from_millis(value)),
        "s" | "" => Ok(Duration::from_secs(value)),
        "m" => Ok(Duration::from_secs(value * 60)),
        "h" => Ok(Duration::from_secs(value * 3600)),
        _ => Err(format!("{raw:?} has an unknown unit, use ms, s, m or h")),
    }
}

impl Options {
    fn presence(&self) -> Result<PresenceConfig, BoxError> {
        let shards = ShardCount::try_from(self.shards)?;
        let lease_ttl = self.lease_ttl.map(LeaseTtl::try_from).transpose()?.unwrap_or_default();
        let heartbeat = self
            .heartbeat_interval
            .map(HeartbeatInterval::try_from)
            .transpose()?
            .unwrap_or_default();
        let marker_ttl = self
            .marker_ttl
            .map(MarkerTtl::try_from)
            .transpose()?
            .unwrap_or_default();
        let config = PresenceConfig::new(self.bucket.clone(), lease_ttl, heartbeat, marker_ttl, shards)?;
        Ok(match &self.jetstream_domain {
            Some(domain) => config.with_domain(domain.clone()),
            None => config,
        })
    }

    fn service(&self) -> Result<ServiceConfig, BoxError> {
        let node = match &self.node_id {
            Some(node) => node.clone(),
            None => NodeId::generate()?,
        };
        let mut config = ServiceConfig::new(self.presence()?, node)
            .with_lease_bucket(self.lease_bucket.clone())
            .with_inflight_batches(self.inflight_batches);
        if let Some(ttl) = self.shard_lease_ttl {
            config = config.with_lease_ttl(ShardLeaseTtl::try_from(ttl)?);
        }
        if let Some(interval) = self.keepalive_interval {
            config = config.with_keepalive(KeepaliveInterval::try_from(interval)?);
        }
        if let Some(window) = self.coalesce_window {
            config = config.with_coalesce(CoalesceWindow::try_from(window)?);
        }
        if let Some(bytes) = self.payload_budget_bytes {
            config =
                config.with_snapshot_limits(SnapshotLimits::default().with_payload(PayloadBudget::try_from(bytes)?));
        }
        Ok(config)
    }

    fn provision_options(&self) -> Result<ProvisionOptions, BoxError> {
        Ok(ProvisionOptions {
            storage: self.storage.into(),
            replicas: Replicas::try_from(self.replicas)?,
        })
    }

    async fn connect(&self) -> Result<async_nats::Client, BoxError> {
        let mut options = async_nats::ConnectOptions::new().name("trogon-presence");
        if let Some(path) = &self.nats_credentials {
            options = options.credentials_file(path).await?;
        }
        Ok(options.connect(self.nats_url.as_str()).await?)
    }
}

async fn execute(cli: Cli) -> Result<(), BoxError> {
    let config = cli.options.service()?;
    let client = cli.options.connect().await?;
    match cli.command {
        Command::Provision => {
            trogon_presence_service::provision(client, &config, cli.options.provision_options()?).await?;
            tracing::info!(bucket = %config.presence().bucket(), lease_bucket = %config.lease_bucket(), "provisioned");
        }
        Command::Run(run) => {
            let config = match run.hook()? {
                Some(hook) => config.with_hook(hook),
                None => config,
            };
            tracing::info!(node = %config.node(), "starting presence service");
            let handle = trogon_presence_service::start(client, config).await?;
            tokio::signal::ctrl_c().await?;
            tracing::info!("shutting down");
            handle.shutdown().await;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let filter = tracing_subscriber::EnvFilter::try_new(&cli.options.log_level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    match execute(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(%err, "trogon-presence failed");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_duration("100ms"), Ok(Duration::from_millis(100)));
        assert_eq!(parse_duration("1m"), Ok(Duration::from_secs(60)));
        assert_eq!(parse_duration("15"), Ok(Duration::from_secs(15)));
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("5d").is_err());
    }

    #[test]
    fn parses_hook_flags() -> Result<(), BoxError> {
        let cli = Cli::try_parse_from([
            "trogon-presence",
            "run",
            "--hook-component",
            "hook.wasm",
            "--hook-policy",
            "fail-open",
            "--hook-deadline",
            "150ms",
            "--hook-memory",
            "32MiB",
            "--hook-http-allow",
            "api.example.com,users.internal",
        ])?;
        let Command::Run(run) = cli.command else {
            return Err("expected the run subcommand".into());
        };
        let hook = run.hook()?.ok_or("expected a hook config")?;
        assert_eq!(hook.component_path, PathBuf::from("hook.wasm"));
        assert_eq!(hook.policy, HookPolicy::FailOpen);
        assert_eq!(hook.deadline.get(), Duration::from_millis(150));
        assert_eq!(hook.memory_limit.bytes(), 32 << 20);
        assert_eq!(hook.http_allow.len(), 2);
        assert!(Cli::try_parse_from(["trogon-presence", "run", "--hook-policy", "fail-open"]).is_err());
        let plain = Cli::try_parse_from(["trogon-presence", "run"])?;
        assert!(matches!(plain.command, Command::Run(run) if run.hook()?.is_none()));
        Ok(())
    }
}
