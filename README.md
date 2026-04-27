# swiss-transport-mcp

MCP stdio server for Swiss public transport data using:
- OJP 2.0 (`/ojp20`) for trip planning, station lookup, and departure boards
- GTFS-RT (`/gtfs-rt`) for real-time trip updates and disruptions
- Train Formation API for coach composition, sectors, and accessibility metadata

## Environment

Create API access in OpenTransportData API Manager:

1. Create an account at `https://api-manager.opentransportdata.swiss/`.
2. Subscribe to all required APIs: OJP2, GTFS-RT, and Train Formation.
3. Create one app and attach all 3 API subscriptions to that app.
4. Generate an app token and use it for this MCP server.

Export tokens in your shell:

```bash
export OJP2_TOKEN="YOUR_OPENTRANSPORTDATA_TOKEN"
export GTFS_RT_TOKEN="YOUR_GTFS_RT_TOKEN"
export FORMATION_TOKEN="YOUR_TRAIN_FORMATION_TOKEN"
export MCP_SERVER_NAME="sbb-transport"
export CACHE_TTL_SECONDS="300"
```

If your API Manager app gives you a single token for all 3 APIs, set all three env vars to that same value.

You can also keep credentials in a `.env` file and load them into the current shell:

```bash
set -a
source .env
set +a
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

## Sample AI Output

### 1) Luzern -> Zürich HB (rail with formation)

Prompt:
`Give me the next train from Luzern to Zürich HB and coach sectors.`

Example AI response:

```text
Best option: IR70
- Departure: 20:10 from Luzern, platform 3
- Arrival: 20:50 at Zürich HB, platform 12
- Duration: 40 min

Boarding hint:
- Go to sector D for coach 2:9 (wheelchair spaces, stroller area)

Coach classes:
- 2nd class: 2:1, 2:2, 2:3, 2:4, 2:5, 2:6, 2:7, 2:8, 2:9
- 1st class: W1:10, 1:11, 1:12, 1:14

Sector mapping:
- B: coach 2:3
- C: coach 2:7
- D: coach 2:9
```

### 2) Deterministic point-to-point (explicit stop IDs)

Prompt:
`Plan rail options from stop 8505000 to 8503000.`

Example AI response:

```text
Resolved route: Luzern (8505000) -> Zürich HB (8503000)

Top 3 rail options:
1) IR70, dep 20:10, arr 20:50, ~40 min
2) IR75, dep 20:35, arr 21:24, ~49 min
3) IR70, dep 21:10, arr 21:50, ~40 min

Use this mode when you need deterministic planning and want to avoid name resolution ambiguity.
```

### 3) Detailed bus ride (single-line city trip)

Prompt:
`Show me a detailed bus itinerary from Bern, Bahnhof to Bern, Bärengraben.`

Example AI response:

```text
Connection: Bus 12 (direct)
- Departure: 19:55 from Bern, Bahnhof (platform A)
- Arrival: 20:01 at Bern, Bärenpark
- Duration: 6 min

Stop-by-stop:
1) Bern, Bahnhof (dep 19:55)
2) Bern, Bärenplatz (arr 19:56)
3) Bern, Zytglogge (arr 19:58)
4) Bern, Rathaus (arr 19:59)
5) Bern, Nydegg (arr 20:00)
6) Bern, Bärenpark (arr 20:01)
```

### 4) Detailed bus ride (multi-leg transfer)

Prompt:
`Give me a bus-only route from Zürich, Central to Zürich, Bucheggplatz with all legs.`

Example AI response:

```text
Trip summary:
- Total duration: ~16 min
- Transfers: 1

Leg 1: Bus 46
- Zürich, Central -> Zürich, Bahnhofquai/HB

Leg 2: Bus 46
- Zürich, Bahnhofquai/HB -> Zürich, Rosengartenstrasse
- Intermediate stops include Stampfenbachplatz, Nordstrasse, Lettenstrasse, Zürich Wipkingen Bahnhof

Leg 3: Bus 83
- Zürich, Rosengartenstrasse -> Zürich, Bucheggplatz (platform G)

Each leg includes realtime expected/scheduled times and platform updates where provided by the feed.
```
