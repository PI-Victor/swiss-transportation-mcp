mod api;
mod cache;
mod config;
mod mcp;
mod models;
mod service;

use anyhow::Result;
use config::Config;
use service::AppState;
use structopt::StructOpt;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = config::Cli::from_args();
    let config = Config::from_cli(cli)?;
    let state = AppState::new(config);
    mcp::run_stdio_server(state).await
}
