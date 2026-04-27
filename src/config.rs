use std::env;

use anyhow::{Result, anyhow, ensure};
use structopt::StructOpt;

#[derive(Debug, Clone, StructOpt)]
#[structopt(name = "swiss-transportation-mcp")]
pub struct Cli {
    #[structopt(long = "api-token", env = "OJP2_TOKEN")]
    pub api_token: Option<String>,

    #[structopt(
        long = "api-token-env",
        env = "OJP2_TOKEN_ENV",
        default_value = "OJP2_TOKEN"
    )]
    pub api_token_env: String,

    #[structopt(long = "gtfs-rt-token", env = "GTFS_RT_TOKEN")]
    pub gtfs_rt_token: Option<String>,

    #[structopt(
        long = "gtfs-rt-token-env",
        env = "GTFS_RT_TOKEN_ENV",
        default_value = "GTFS_RT_TOKEN"
    )]
    pub gtfs_rt_token_env: String,

    #[structopt(
        long = "ojp-endpoint",
        env = "OJP2_ENDPOINT",
        default_value = "https://api.opentransportdata.swiss/ojp20"
    )]
    pub ojp_endpoint: String,

    #[structopt(
        long = "gtfs-rt-endpoint",
        env = "GTFS_RT_ENDPOINT",
        default_value = "https://api.opentransportdata.swiss/la/gtfs-rt"
    )]
    pub gtfs_rt_endpoint: String,

    #[structopt(
        long = "formation-endpoint",
        env = "FORMATION_ENDPOINT",
        default_value = "https://api.opentransportdata.swiss/formation/v2/formations_stop_based"
    )]
    pub formation_endpoint: String,

    #[structopt(long = "formation-token", env = "FORMATION_TOKEN")]
    pub formation_token: Option<String>,

    #[structopt(
        long = "formation-token-env",
        env = "FORMATION_TOKEN_ENV",
        default_value = "FORMATION_TOKEN"
    )]
    pub formation_token_env: String,

    #[structopt(
        long = "server-name",
        env = "MCP_SERVER_NAME",
        default_value = "sbb-transport"
    )]
    pub server_name: String,

    #[structopt(
        long = "cache-ttl-seconds",
        env = "CACHE_TTL_SECONDS",
        default_value = "300"
    )]
    pub cache_ttl_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub ojp_endpoint: String,
    pub gtfs_rt_endpoint: String,
    pub formation_endpoint: String,
    pub api_token: String,
    pub gtfs_rt_token: String,
    pub formation_token: String,
    pub server_name: String,
    pub cache_ttl_seconds: u64,
}

impl Config {
    pub fn from_cli(cli: Cli) -> Result<Self> {
        ensure!(
            cli.cache_ttl_seconds > 0,
            "cache ttl seconds must be greater than 0"
        );
        let api_token = match cli.api_token {
            Some(token) => token,
            None => env::var(&cli.api_token_env).map_err(|_| {
                anyhow!(
                    "missing OJP token: set {} or pass --api-token (Codex MCP stdio: whitelist this env via mcp_servers.<id>.env_vars)",
                    cli.api_token_env
                )
            })?,
        };
        ensure!(!api_token.trim().is_empty(), "OJP token must not be empty");

        let gtfs_rt_token = match cli.gtfs_rt_token {
            Some(token) => token,
            None => env::var(&cli.gtfs_rt_token_env).map_err(|_| {
                anyhow!(
                    "missing GTFS-RT token: set {} or pass --gtfs-rt-token (Codex MCP stdio: whitelist this env via mcp_servers.<id>.env_vars)",
                    cli.gtfs_rt_token_env
                )
            })?,
        };
        ensure!(
            !gtfs_rt_token.trim().is_empty(),
            "GTFS-RT token must not be empty"
        );

        let formation_token = match cli.formation_token {
            Some(token) => token,
            None => env::var(&cli.formation_token_env).map_err(|_| {
                anyhow!(
                    "missing Train Formation token: set {} or pass --formation-token (Codex MCP stdio: whitelist this env via mcp_servers.<id>.env_vars)",
                    cli.formation_token_env
                )
            })?,
        };
        ensure!(
            !formation_token.trim().is_empty(),
            "Train Formation token must not be empty"
        );

        Ok(Self {
            ojp_endpoint: cli.ojp_endpoint,
            gtfs_rt_endpoint: cli.gtfs_rt_endpoint,
            formation_endpoint: cli.formation_endpoint,
            gtfs_rt_token,
            formation_token,
            api_token,
            server_name: cli.server_name,
            cache_ttl_seconds: cli.cache_ttl_seconds,
        })
    }
}
