# swiss-transport-mcp

MCP stdio server for Swiss public transport data using:
- OJP 2.0 (`/ojp20`) for trip planning, station lookup, and departure boards
- GTFS-RT (`/gtfs-rt`) for real-time trip updates and disruptions

## Environment

Create an API token in OpenTransportData and export it:

```bash
export OJP2_TOKEN="YOUR_OPENTRANSPORTDATA_TOKEN"
export GTFS_RT_TOKEN="YOUR_GTFS_RT_TOKEN"
export FORMATION_TOKEN="YOUR_TRAIN_FORMATION_TOKEN"
export MCP_SERVER_NAME="sbb-transport"
export CACHE_TTL_SECONDS="300"
```

Optional endpoint overrides:

```bash
export OJP2_ENDPOINT="https://api.opentransportdata.swiss/ojp20"
export GTFS_RT_ENDPOINT="https://api.opentransportdata.swiss/la/gtfs-rt"
```

## Run

Token is read from `OJP2_TOKEN` by default:

```bash
swiss-transportation-mcp
```

The process speaks MCP JSON-RPC on stdio with `Content-Length` framing.

CLI is parsed with `structopt`; flags override env values:

```bash
swiss-transportation-mcp \
  --api-token "$OJP2_TOKEN" \
  --server-name "sbb-transport"
```

Show all CLI options:

```bash
swiss-transportation-mcp --help
```

## Add To Codex MCP

Add this MCP server entry to your Codex config at `~/.codex/config.toml`:

```toml
[mcp_servers.sbb_transport]
command = "swiss-transportation-mcp"
args = []
env = { OJP2_TOKEN_ENV = "OJP2_TOKEN", GTFS_RT_TOKEN_ENV = "GTFS_RT_TOKEN", FORMATION_TOKEN_ENV = "FORMATION_TOKEN", MCP_SERVER_NAME = "sbb-transport", CACHE_TTL_SECONDS = "300" }
env_vars = ["OJP2_TOKEN", "GTFS_RT_TOKEN", "FORMATION_TOKEN"]
```

### Token Loading In AI Agents

Yes, AI agents (including Codex) fetch the token from the MCP server process environment.
This server reads the token env var names from `OJP2_TOKEN_ENV`, `GTFS_RT_TOKEN_ENV`, and `FORMATION_TOKEN_ENV`.
Defaults are `OJP2_TOKEN`, `GTFS_RT_TOKEN`, and `FORMATION_TOKEN`.

Export the real token env vars in your shell before starting Codex so the MCP process inherits them:

```bash
export OJP2_TOKEN="YOUR_OPENTRANSPORTDATA_TOKEN"
export GTFS_RT_TOKEN="YOUR_GTFS_RT_TOKEN"
export FORMATION_TOKEN="YOUR_TRAIN_FORMATION_TOKEN"
```

In Codex CLI, secret-like variables are excluded from inherited env by default.
`env_vars = ["OJP2_TOKEN", "GTFS_RT_TOKEN", "FORMATION_TOKEN"]` is required so this MCP stdio server receives those variables.
If Codex cannot find `swiss-transportation-mcp`, enable shell profile inheritance so `~/.cargo/bin` is available:

```toml
[shell_environment_policy]
experimental_use_profile = true
```

If `OJP2_TOKEN` is not present (and `--api-token` is not passed), startup fails with:
`missing OJP token: set <value of OJP2_TOKEN_ENV> or pass --api-token`.

If `GTFS_RT_TOKEN` is not present (and `--gtfs-rt-token` is not passed), startup fails.

If `FORMATION_TOKEN` is not present (and `--formation-token` is not passed), startup fails.

Restart Codex after saving the config so the MCP server is discovered and its tools are loaded.

## Implemented MCP Tools

- `search_stations(query, limit)`
- `plan_trip(from_station, to_station, datetime, modes)`
- `get_departures(station_id, limit, time_window)`
- `get_trip_details(trip_id)`
- `monitor_trip(trip_id, notify_on_delay_minutes)`
- `list_transport_modes()`
- `get_disruptions(line?, station?)`

## Example Requests

See [`examples/tool_calls.json`](/Users/vicp/projects/rust/swiss-transport-mcp/examples/tool_calls.json) for ready-to-send `tools/call` payloads.
