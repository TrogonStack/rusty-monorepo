use clap::Parser;

use trg::cli::Cli;
use trg::commands::ai::AiCommands;
use trg::commands::mcp::{report_startup_failure, McpCommands, McpContext};
use trg::commands::Commands;
use trg::config;
use trg::secrets::{vars, CredentialPathError, Registry, ServerBackendError, VarFetchError};

#[derive(Debug, thiserror::Error)]
enum WireError {
    #[error("{0}")]
    Config(#[from] config::ConfigError),

    #[error("{0}")]
    Backend(#[from] ServerBackendError),

    #[error("{0}")]
    CredentialPath(#[from] CredentialPathError),

    #[error("{0}")]
    VarFetch(#[from] VarFetchError),
}

/// Resolve everything `trg mcp` depends on. This is the only place that reads
/// config or picks a secrets backend.
///
/// The config is read in two steps with the backend reads in between, so the
/// file is parsed and validated before anything needs to be reachable, and
/// only a server that actually declares a `{ backend = ... }` var pays for one.
///
/// Where the endpoint sits behind such a var, resolving it is a network read,
/// and `auth logout` is the documented way out of a credential that will not
/// load. So the commands that only touch stored credentials stop before the
/// fetch: an unreachable backend takes the proxy down without also taking down
/// the command that recovers from it.
async fn wire_mcp(command: &McpCommands) -> Result<McpContext, Box<WireError>> {
    let server_name = command.server_name().to_string();
    let pending = config::load_mcp(&server_name).map_err(WireError::from)?;
    let registry = Registry::new(pending.secrets.clone());

    let backend = registry
        .for_server(&server_name, pending.server_secrets())
        .map_err(WireError::from)?;
    let cred_path = backend.credential_path(&server_name).map_err(WireError::from)?;

    if !command.needs_endpoint() {
        return Ok(McpContext::credentials_only(server_name, backend, cred_path));
    }

    let fetched = vars::fetch(&registry, &pending.secret_vars())
        .await
        .map_err(WireError::from)?;
    let loaded = pending.finish(&fetched).map_err(WireError::from)?;

    Ok(McpContext::with_endpoint(
        server_name,
        backend,
        cred_path,
        loaded.server,
    ))
}

/// `trg doctor` reads the config for the backends alone, since a config that
/// declares one before declaring anything that uses it is still a config this
/// command can answer about.
fn wire_secrets() -> Result<Registry, Box<WireError>> {
    let section = config::load_secrets().map_err(WireError::from)?;
    Ok(Registry::new(section))
}

/// Resolve one `[exec.<name>]` entry, the same two-step shape as `wire_mcp`:
/// the config is parsed and the entry located before a secrets backend needs
/// to be reachable.
async fn wire_exec(name: &str) -> Result<config::LoadedExec, Box<WireError>> {
    let pending = config::load_exec(name).map_err(WireError::from)?;
    let registry = Registry::new(pending.secrets.clone());
    let fetched = vars::fetch(&registry, &pending.secret_vars())
        .await
        .map_err(WireError::from)?;
    pending.finish(&fetched).map_err(|e| Box::new(WireError::from(e)))
}

#[tokio::main]
async fn main() {
    trg::telemetry::init();

    let cli = Cli::parse();
    let fs = trg::fs::RealFS;

    let exit_code = match cli.command {
        Commands::Ai { command } => match command {
            AiCommands::Skills { command } => command.handle(&fs),
        },
        Commands::Mcp { command } => match wire_mcp(&command).await {
            Ok(ctx) => command.handle(&ctx).await,
            Err(e) => report_startup_failure(&command, &e).await,
        },
        Commands::Secret { command } => match wire_secrets() {
            Ok(registry) => command.handle(&registry).await,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        Commands::Doctor(args) => match wire_secrets() {
            Ok(registry) => trg::commands::doctor::run(&registry, &args).await,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        Commands::Exec(args) => match wire_exec(&args.name).await {
            Ok(loaded) => trg::commands::exec::run(loaded, &args),
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
    };

    std::process::exit(exit_code);
}
