//! `trg secrets`: inspect the configured secrets backends.

pub mod doctor;

use clap::{Args, Subcommand, ValueEnum};

use crate::secrets::Registry;

#[derive(Subcommand)]
pub enum SecretsCommands {
    /// Check that a configured backend is reachable and usable.
    Doctor(DoctorArgs),
}

#[derive(Args, Debug, Clone)]
pub struct DoctorArgs {
    /// Backend name as it appears under `[secrets.backends.<name>]`.
    #[arg(long)]
    pub backend: String,

    /// Output format. Neither format emits a secret value.
    #[arg(long, value_enum, default_value_t = DoctorFormat::Text)]
    pub format: DoctorFormat,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum DoctorFormat {
    Text,
    Json,
}

impl SecretsCommands {
    pub async fn handle(self, registry: &Registry) -> i32 {
        match self {
            Self::Doctor(args) => run_doctor(registry, &args).await,
        }
    }
}

async fn run_doctor(registry: &Registry, args: &DoctorArgs) -> i32 {
    let backend = match registry.resolve(&args.backend) {
        Ok(backend) => backend,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    let report = doctor::diagnose(&args.backend, &backend).await;

    match args.format {
        DoctorFormat::Text => print!("{}", report.to_text()),
        DoctorFormat::Json => match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("could not render the report: {e}");
                return 1;
            }
        },
    }

    report.exit_code()
}
