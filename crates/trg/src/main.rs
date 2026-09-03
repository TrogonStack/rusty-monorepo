use clap::Parser;

use trg::commands::ai::AiCommands;
use trg::commands::mcp::{McpCommands, McpContext};
use trg::commands::Commands;
use trg::config;
use trg::secrets::{Backend, KeychainBackend, PathError, SecretPath};

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

    #[error("server name is not usable as a secret path: {0}")]
    CredentialPath(#[from] PathError),
}

/// Resolve everything `trg mcp` depends on. This is the only place that reads
/// config or picks a secrets backend.
fn wire_mcp(command: &McpCommands) -> Result<McpContext, WireError> {
    let server_name = command.server_name().to_string();
    let profile = config::load_mcp_server(&server_name)?;
    let cred_path = SecretPath::parse(&server_name)?;
    let backend = Backend::Keychain(KeychainBackend::with_default_service());

    Ok(McpContext {
        server_name,
        profile,
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
