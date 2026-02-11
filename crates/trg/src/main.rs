use clap::Parser;

use trg::commands::ai::AiCommands;
use trg::commands::Commands;

#[derive(Parser)]
#[command(name = "trg")]
#[command(about = "TrogonStack tools and utilities")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

fn main() {
    let cli = Cli::parse();
    let fs = trg::fs::RealFS;

    let exit_code = match cli.command {
        Commands::Ai { command } => match command {
            AiCommands::Skills { command } => command.handle(&fs),
        },
    };

    std::process::exit(exit_code);
}
