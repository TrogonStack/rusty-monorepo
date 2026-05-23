use clap::Args;

/// `trg mcp proxy` flags (`--param value`, no positional args).
#[derive(Args, Debug, Clone)]
pub struct ProxyArgs {
    /// Select `[mcp.servers.<name>]` from `trg`'s configuration file (required).
    #[arg(long)]
    pub server: String,
}
