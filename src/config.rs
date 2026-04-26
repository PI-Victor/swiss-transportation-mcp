use anyhow::{Result, ensure};
use structopt::StructOpt;

#[derive(Debug, Clone, StructOpt)]
#[structopt(name = "swiss-transport-mcp")]
pub struct Cli {
    #[structopt(long = "api-token", env = "SBB_API_TOKEN")]
    pub api_token: String,

    #[structopt(long = "gtfs-rt-token", env = "SBB_GTFS_RT_TOKEN")]
    pub gtfs_rt_token: Option<String>,

    #[structopt(
        long = "ojp-endpoint",
        env = "SBB_OJP_ENDPOINT",
        default_value = "https://api.opentransportdata.swiss/ojp20"
    )]
    pub ojp_endpoint: String,

    #[structopt(
        long = "gtfs-rt-endpoint",
        env = "SBB_GTFS_RT_ENDPOINT",
        default_value = "https://api.opentransportdata.swiss/gtfs-rt"
    )]
    pub gtfs_rt_endpoint: String,

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
    pub api_token: String,
    pub gtfs_rt_token: String,
    pub server_name: String,
    pub cache_ttl_seconds: u64,
}

impl Config {
    pub fn from_cli(cli: Cli) -> Result<Self> {
        ensure!(
            cli.cache_ttl_seconds > 0,
            "cache ttl seconds must be greater than 0"
        );

        Ok(Self {
            ojp_endpoint: cli.ojp_endpoint,
            gtfs_rt_endpoint: cli.gtfs_rt_endpoint,
            gtfs_rt_token: cli.gtfs_rt_token.unwrap_or_else(|| cli.api_token.clone()),
            api_token: cli.api_token,
            server_name: cli.server_name,
            cache_ttl_seconds: cli.cache_ttl_seconds,
        })
    }
}
