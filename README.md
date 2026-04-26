# swiss-transport-mcp

MCP stdio server for Swiss public transport data using:
- OJP 2.0 (`/ojp20`) for trip planning, station lookup, and departure boards
- GTFS-RT (`/gtfs-rt`) for real-time trip updates and disruptions

## Environment

```bash
export SBB_API_TOKEN="..."
export SBB_GTFS_RT_TOKEN="..."   # optional, defaults to SBB_API_TOKEN
export MCP_SERVER_NAME="sbb-transport"
export CACHE_TTL_SECONDS="300"
```

Optional endpoint overrides:

```bash
export SBB_OJP_ENDPOINT="https://api.opentransportdata.swiss/ojp20"
export SBB_GTFS_RT_ENDPOINT="https://api.opentransportdata.swiss/gtfs-rt"
```

## Run

```bash
cargo run
```

The process speaks MCP JSON-RPC on stdio with `Content-Length` framing.

CLI is parsed with `structopt`; flags override env values:

```bash
cargo run -- \
  --api-token "$SBB_API_TOKEN" \
  --server-name "sbb-transport"
```

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
