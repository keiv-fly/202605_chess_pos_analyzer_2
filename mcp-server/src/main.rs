use anyhow::Result;
use clap::Parser;
use rmcp::{transport::stdio, ServiceExt};
use tracing_subscriber::EnvFilter;

use chess_pos_analyzer::config::{CliArgs, ResolvedConfig};
use chess_pos_analyzer::server::ChessServer;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = CliArgs::parse();
    let config = ResolvedConfig::resolve(cli)?;

    // Logs go to stderr so stdout stays reserved for MCP framing.
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!(
        stockfish_path = %config.stockfish_path.display(),
        cache_capacity = config.cache_capacity,
        "starting chess-pos-analyzer MCP server over stdio"
    );

    let server = ChessServer::new(config);
    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!(error = ?e, "failed to start MCP service");
    })?;
    service.waiting().await?;
    Ok(())
}
