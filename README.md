# swiss-transport-mcp

MCP stdio server for Swiss public transport data using:
- OJP 2.0 (`/ojp20`) for trip planning, station lookup, and departure boards
- GTFS-RT (`/gtfs-rt`) for real-time trip updates and disruptions

## Environment

Create an API token in OpenTransportData and export it:

```bash
export OJP2_TOKEN="YOUR_OPENTRANSPORTDATA_TOKEN"
# optional, only if GTFS-RT uses a different token
export SBB_GTFS_RT_TOKEN="YOUR_GTFS_RT_TOKEN"
export MCP_SERVER_NAME="sbb-transport"
export CACHE_TTL_SECONDS="300"
```

Optional endpoint overrides:

```bash
export SBB_OJP_ENDPOINT="https://api.opentransportdata.swiss/ojp20"
export SBB_GTFS_RT_ENDPOINT="https://api.opentransportdata.swiss/gtfs-rt"
```

## Run

`--api-token` is required (or `OJP2_TOKEN` env var must be set):

```bash
cargo run
```

The process speaks MCP JSON-RPC on stdio with `Content-Length` framing.

CLI is parsed with `structopt`; flags override env values:

```bash
cargo run -- \
  --api-token "$OJP2_TOKEN" \
  --server-name "sbb-transport"
```

Show all CLI options:

```bash
cargo run -- --help
```

## Add To Codex MCP

Add this MCP server entry to your Codex config at `~/.codex/config.toml`:

```toml
[mcp_servers.sbb_transport]
command = "cargo"
args = ["run", "--manifest-path", "/Users/vicp/projects/rust/swiss-transport-mcp/Cargo.toml", "--quiet", "--"]
env = { OJP2_TOKEN = "YOUR_OPENTRANSPORTDATA_TOKEN", SBB_GTFS_RT_TOKEN = "YOUR_GTFS_RT_TOKEN", MCP_SERVER_NAME = "sbb-transport", CACHE_TTL_SECONDS = "300" }
```

If you use one token for both endpoints, remove `SBB_GTFS_RT_TOKEN` from the `env` table.
`OJP2_TOKEN` should contain your OJP token.

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
