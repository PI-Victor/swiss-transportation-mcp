use std::collections::HashMap;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{
    self, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::mpsc;

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

pub async fn run_stdio_server(state: AppState) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin);

    let (writer_tx, mut writer_rx) = mpsc::channel::<Value>(128);

    tokio::spawn(async move {
        let mut stdout = stdout;
        while let Some(message) = writer_rx.recv().await {
            if write_framed_message(&mut stdout, &message).await.is_err() {
                break;
            }
        }
    });

    let monitor_state = state.clone();
    let monitor_writer = writer_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            if let Ok(notifications) = monitor_state.monitor_notifications().await {
                for notification in notifications {
                    if monitor_writer.send(notification).await.is_err() {
                        return;
                    }
                }
            }
        }
    });

    while let Some(message) = read_framed_message(&mut reader).await? {
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
            .send(serde_json::to_value(response)?)
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

async fn read_framed_message<R>(reader: &mut BufReader<R>) -> Result<Option<Value>>
where
    R: AsyncRead + Unpin,
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

async fn write_framed_message<W>(writer: &mut W, value: &Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(value)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(&body).await?;
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
            "name": "plan_trip",
            "description": "Plan a trip between two stations using OJP trip planning",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from_station": { "type": "string" },
                    "to_station": { "type": "string" },
                    "datetime": { "type": "string", "description": "ISO8601, earliest, now, tomorrow at 6" },
                    "modes": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "query": { "type": "string" }
                },
                "required": ["from_station"]
            }
        }),
        json!({
            "name": "get_departures",
            "description": "Get real-time departures for a station",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "station_id": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "time_window": { "type": "integer", "minimum": 1, "description": "minutes" }
                },
                "required": ["station_id"]
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
