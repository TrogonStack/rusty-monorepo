use clap::Parser;

use trg::commands::ai::AiCommands;
use trg::commands::mcp::{McpCommands, McpContext};
use trg::commands::Commands;
use trg::config;
use trg::secrets::{CredentialPathError, Registry, ServerBackendError};

#[derive(Parser)]
#[command(name = "trg")]
#[command(about = "TrogonStack tools and utilities")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, thiserror::Error)]
enum WireError {
    #[error("{0}")]
    Config(#[from] config::ConfigError),

    #[error("{0}")]
    Backend(#[from] ServerBackendError),

    #[error("{0}")]
    CredentialPath(#[from] CredentialPathError),
}

/// Resolve everything `trg mcp` depends on. This is the only place that reads
/// config or picks a secrets backend.
fn wire_mcp(command: &McpCommands) -> Result<McpContext, Box<WireError>> {
    let server_name = command.server_name().to_string();
    let loaded = config::load_mcp(&server_name).map_err(WireError::from)?;
    let registry = Registry::new(loaded.secrets);

    let backend = registry
        .for_server(&server_name, loaded.server.secrets.as_deref())
        .map_err(WireError::from)?;

    let cred_path = backend.credential_path(&server_name).map_err(WireError::from)?;

    Ok(McpContext {
        server_name,
        profile: loaded.server,
        backend,
        cred_path,
    })
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
        Commands::Mcp { command } => match wire_mcp(&command) {
            Ok(ctx) => command.handle(&ctx).await,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
    };

    std::process::exit(exit_code);
}
