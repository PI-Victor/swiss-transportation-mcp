use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{
    self, AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::{RwLock, mpsc};

use crate::service::AppState;

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FramingMode {
    ContentLength,
    NewlineDelimited,
}

#[derive(Debug)]
struct OutboundMessage {
    value: Value,
    framing: FramingMode,
}

pub async fn run_stdio_server(state: AppState) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin);

    let (writer_tx, mut writer_rx) = mpsc::channel::<OutboundMessage>(128);
    let framing_mode = Arc::new(RwLock::new(FramingMode::ContentLength));

    tokio::spawn(async move {
        let mut stdout = stdout;
        while let Some(message) = writer_rx.recv().await {
            if write_message(&mut stdout, &message.value, message.framing)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let monitor_state = state.clone();
    let monitor_writer = writer_tx.clone();
    let monitor_framing = framing_mode.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            if let Ok(notifications) = monitor_state.monitor_notifications().await {
                let framing = *monitor_framing.read().await;
                for notification in notifications {
                    if monitor_writer
                        .send(OutboundMessage {
                            value: notification,
                            framing,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    });

    let mut framing = None;
    while let Some(message) = read_message(&mut reader, &mut framing).await? {
        let message_framing = framing.unwrap_or(FramingMode::ContentLength);
        {
            let mut current_framing = framing_mode.write().await;
            *current_framing = message_framing;
        }

        let request: JsonRpcRequest = serde_json::from_value(message)?;

        if request.method == "notifications/initialized" {
            continue;
        }

        let response = match handle_request(&state, request).await {
            Ok(Some(response)) => response,
            Ok(None) => continue,
            Err(err) => JsonRpcResponse {
                jsonrpc: "2.0",
                id: Value::Null,
                result: None,
                error: Some(JsonRpcError {
                    code: -32000,
                    message: err.to_string(),
                    data: None,
                }),
            },
        };

        writer_tx
            .send(OutboundMessage {
                value: serde_json::to_value(response)?,
                framing: message_framing,
            })
            .await
            .map_err(|_| anyhow!("writer task ended"))?;
    }

    Ok(())
}

async fn handle_request(
    state: &AppState,
    request: JsonRpcRequest,
) -> Result<Option<JsonRpcResponse>> {
    let id = request.id.unwrap_or(Value::Null);

    let response = match request.method.as_str() {
        "initialize" => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {
                        "listChanged": false
                    }
                },
                "serverInfo": {
                    "name": state.config.server_name,
                    "version": env!("CARGO_PKG_VERSION")
                }
            })),
            error: None,
        },
        "ping" => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({})),
            error: None,
        },
        "tools/list" => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({ "tools": tool_definitions() })),
            error: None,
        },
        "tools/call" => {
            let params = request.params.unwrap_or(Value::Null);
            let tool_name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("tools/call requires 'name'"))?;
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));

            let call_result = state.call_tool(tool_name, arguments).await;
            let result = match call_result {
                Ok(payload) => {
                    let rendered = serde_json::to_string_pretty(&payload)?;
                    json!({
                        "content": [
                            {
                                "type": "text",
                                "text": rendered
                            }
                        ],
                        "structuredContent": payload,
                        "isError": false
                    })
                }
                Err(err) => {
                    json!({
                        "content": [
                            {
                                "type": "text",
                                "text": err.to_string()
                            }
                        ],
                        "structuredContent": {
                            "error": err.to_string(),
                        },
                        "isError": true
                    })
                }
            };

            JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(result),
                error: None,
            }
        }
        _ => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("method not found: {}", request.method),
                data: None,
            }),
        },
    };

    Ok(Some(response))
}

async fn read_message<R>(reader: &mut R, framing: &mut Option<FramingMode>) -> Result<Option<Value>>
where
    R: AsyncBufRead + Unpin,
{
    let mode = match framing {
        Some(mode) => *mode,
        None => match detect_framing(reader).await? {
            Some(mode) => {
                *framing = Some(mode);
                mode
            }
            None => return Ok(None),
        },
    };

    match mode {
        FramingMode::ContentLength => read_content_length_message(reader).await,
        FramingMode::NewlineDelimited => read_newline_message(reader).await,
    }
}

async fn detect_framing<R>(reader: &mut R) -> Result<Option<FramingMode>>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok(None);
        }
        match buffer[0] {
            b'{' => return Ok(Some(FramingMode::NewlineDelimited)),
            b'C' | b'c' => return Ok(Some(FramingMode::ContentLength)),
            b'\n' | b'\r' | b' ' | b'\t' => reader.consume(1),
            _ => return Ok(Some(FramingMode::NewlineDelimited)),
        }
    }
}

async fn read_content_length_message<R>(reader: &mut R) -> Result<Option<Value>>
where
    R: AsyncBufRead + Unpin,
{
    let mut headers = HashMap::new();
    loop {
        let mut line = Vec::new();
        let bytes = reader.read_until(b'\n', &mut line).await?;
        if bytes == 0 {
            return Ok(None);
        }

        if line == b"\r\n" || line == b"\n" {
            break;
        }

        let line = String::from_utf8_lossy(&line);
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let content_length = headers
        .get("content-length")
        .ok_or_else(|| anyhow!("missing Content-Length header"))?
        .parse::<usize>()
        .map_err(|_| anyhow!("invalid Content-Length header"))?;

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    let value = serde_json::from_slice(&body)?;
    Ok(Some(value))
}

async fn read_newline_message<R>(reader: &mut R) -> Result<Option<Value>>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let mut line = Vec::new();
        let bytes = reader.read_until(b'\n', &mut line).await?;
        if bytes == 0 {
            return Ok(None);
        }
        let line = String::from_utf8_lossy(&line);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value = serde_json::from_str(trimmed)?;
        return Ok(Some(value));
    }
}

async fn write_message<W>(writer: &mut W, value: &Value, framing: FramingMode) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(value)?;
    match framing {
        FramingMode::ContentLength => {
            let header = format!("Content-Length: {}\r\n\r\n", body.len());
            writer.write_all(header.as_bytes()).await?;
            writer.write_all(&body).await?;
        }
        FramingMode::NewlineDelimited => {
            writer.write_all(&body).await?;
            writer.write_all(b"\n").await?;
        }
    }
    writer.flush().await?;
    Ok(())
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "search_stations",
            "description": "Search Swiss stations and stops by free text query",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "resolve_place",
            "description": "Resolve a place/station query to OJP ids for deterministic point-to-point trip planning",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 20 },
                    "strict_exact": {
                        "type": "boolean",
                        "description": "if true, fail unless one result exactly matches the query name"
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "plan_trip",
            "description": "Plan a trip between places/stations using OJP trip planning",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from_station": {
                        "type": "string",
                        "description": "origin station/place name (required when from_station_id is not provided)"
                    },
                    "from_station_id": { "type": "string", "description": "optional explicit origin stop/place id" },
                    "to_station": {
                        "type": "string",
                        "description": "destination station/place name (required when to_station_id is not provided)"
                    },
                    "to_station_id": { "type": "string", "description": "optional explicit destination stop/place id" },
                    "datetime": { "type": "string", "description": "ISO8601, earliest, now, tomorrow at 6" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 5, "description": "maximum number of options to return (default 5)" },
                    "strict_resolution": {
                        "type": "boolean",
                        "description": "if true and no explicit id is provided, require exact station-name match instead of best-effort fuzzy selection"
                    },
                    "modes": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }
            }
        }),
        json!({
            "name": "plan_trip_point_to_point",
            "description": "Plan a deterministic trip between explicit origin/destination ids",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from_id": { "type": "string" },
                    "to_id": { "type": "string" },
                    "from_name": { "type": "string", "description": "optional human-readable origin label" },
                    "to_name": { "type": "string", "description": "optional human-readable destination label" },
                    "datetime": { "type": "string", "description": "ISO8601, earliest, now, tomorrow at 6" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 5, "description": "maximum number of options to return (default 5)" },
                    "modes": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["from_id", "to_id"]
            }
        }),
        json!({
            "name": "get_departures",
            "description": "Get real-time departures for a station id or station query (supports mode filters, e.g. boats/ships)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "station_id": { "type": "string" },
                    "station_query": { "type": "string", "description": "free-text place/station query when station_id is not provided" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "time_window": { "type": "integer", "minimum": 1, "description": "minutes" },
                    "modes": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "query": { "type": "string", "description": "free text like 'boats in the area'" }
                }
            }
        }),
        json!({
            "name": "get_trip_details",
            "description": "Get live GTFS-RT updates for a specific trip",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "trip_id": { "type": "string" }
                },
                "required": ["trip_id"]
            }
        }),
        json!({
            "name": "monitor_trip",
            "description": "Subscribe to delay notifications for a trip",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "trip_id": { "type": "string" },
                    "notify_on_delay_minutes": { "type": "integer", "minimum": 1 }
                },
                "required": ["trip_id"]
            }
        }),
        json!({
            "name": "list_transport_modes",
            "description": "List supported transport mode filters",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "get_disruptions",
            "description": "Get active service disruptions from GTFS alerts",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "line": { "type": "string" },
                    "station": { "type": "string" }
                }
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::tool_definitions;

    #[test]
    fn tool_input_schemas_are_top_level_objects_without_disallowed_keywords() {
        let tools = tool_definitions();
        let disallowed = ["oneOf", "anyOf", "allOf", "enum", "not"];

        for tool in tools {
            let name = tool
                .get("name")
                .and_then(|value| value.as_str())
                .expect("tool definition must include string name");
            let schema = tool
                .get("inputSchema")
                .and_then(|value| value.as_object())
                .expect("tool definition must include object inputSchema");
            let schema_type = schema
                .get("type")
                .and_then(|value| value.as_str())
                .expect("inputSchema must include string type");

            assert_eq!(
                schema_type, "object",
                "tool `{name}` inputSchema top-level type must be object"
            );

            for keyword in disallowed {
                assert!(
                    !schema.contains_key(keyword),
                    "tool `{name}` inputSchema must not contain top-level `{keyword}`"
                );
            }
        }
    }

    #[test]
    fn plan_trip_schema_stays_validator_compatible() {
        let plan_trip = tool_definitions()
            .into_iter()
            .find(|tool| tool.get("name").and_then(|value| value.as_str()) == Some("plan_trip"))
            .expect("plan_trip tool definition must exist");

        let schema = plan_trip
            .get("inputSchema")
            .and_then(|value| value.as_object())
            .expect("plan_trip inputSchema must be an object");

        assert_eq!(
            schema.get("type").and_then(|value| value.as_str()),
            Some("object")
        );
        assert!(!schema.contains_key("anyOf"));
    }
}
