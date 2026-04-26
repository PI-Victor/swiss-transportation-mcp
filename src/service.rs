use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use chrono::{FixedOffset, Utc};
use chrono_tz::Europe::Zurich;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::RwLock;

use crate::api::gtfs_rt::GtfsRtClient;
use crate::api::ojp::OjpClient;
use crate::config::Config;
use crate::models::{
    DeparturesResponse, DisruptionsResponse, MonitorSubscription, StationsResponse, ToolWarning,
    TransportMode, TripDetailsResponse, TripsResponse,
};
use crate::nl::{parse_datetime_input, parse_modes, split_route_phrase};

#[derive(Debug, Clone)]
struct SubscriptionState {
    trip_id: String,
    threshold_minutes: i32,
    last_reported_delay_minutes: i32,
    created_at: chrono::DateTime<FixedOffset>,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub ojp: OjpClient,
    pub gtfs_rt: GtfsRtClient,
    subscriptions: Arc<RwLock<HashMap<String, SubscriptionState>>>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let _cache_ttl_seconds = config.cache_ttl_seconds;
        let ojp = OjpClient::new(config.ojp_endpoint.clone(), config.api_token.clone());
        let gtfs_rt = GtfsRtClient::new(
            config.gtfs_rt_endpoint.clone(),
            config.gtfs_rt_token.clone(),
        );

        Self {
            config,
            ojp,
            gtfs_rt,
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value> {
        match tool {
            "search_stations" => self.tool_search_stations(arguments).await,
            "plan_trip" => self.tool_plan_trip(arguments).await,
            "get_departures" => self.tool_get_departures(arguments).await,
            "get_trip_details" => self.tool_get_trip_details(arguments).await,
            "monitor_trip" => self.tool_monitor_trip(arguments).await,
            "list_transport_modes" => self.tool_list_transport_modes().await,
            "get_disruptions" => self.tool_get_disruptions(arguments).await,
            _ => Err(anyhow!("unknown tool: {tool}")),
        }
    }

    pub async fn monitor_notifications(&self) -> Result<Vec<Value>> {
        let snapshot = {
            let subscriptions = self.subscriptions.read().await;
            subscriptions
                .iter()
                .map(|(id, sub)| (id.clone(), sub.clone()))
                .collect::<Vec<_>>()
        };

        let mut notifications = Vec::new();

        for (subscription_id, subscription) in snapshot {
            let (details, warning) = self.gtfs_rt.trip_details(&subscription.trip_id).await?;
            let Some(details) = details else {
                continue;
            };

            if details.current_delay_minutes >= subscription.threshold_minutes
                && details.current_delay_minutes > subscription.last_reported_delay_minutes
            {
                notifications.push(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/message",
                    "params": {
                        "level": "info",
                        "data": {
                            "subscriptionId": subscription_id,
                            "tripId": subscription.trip_id,
                            "delayMinutes": details.current_delay_minutes,
                            "thresholdMinutes": subscription.threshold_minutes,
                            "warning": warning,
                        }
                    }
                }));

                let mut subscriptions = self.subscriptions.write().await;
                if let Some(entry) = subscriptions.get_mut(&subscription_id) {
                    entry.last_reported_delay_minutes = details.current_delay_minutes;
                }
            }
        }

        Ok(notifications)
    }

    async fn tool_search_stations(&self, arguments: Value) -> Result<Value> {
        #[derive(Debug, Deserialize)]
        struct Args {
            query: String,
            limit: Option<usize>,
        }

        let args: Args = serde_json::from_value(arguments)?;
        let limit = args.limit.unwrap_or(10).min(50);

        let (stations, suggestions, warning) = self.ojp.search_stations(&args.query, limit).await?;

        if stations.is_empty() {
            return Err(anyhow!(
                "{} (suggestions: {})",
                args.query,
                suggestions.join(", ")
            ));
        }

        let response = StationsResponse {
            stations,
            suggestions,
        };

        Ok(json!({
            "result": response,
            "warning": warning,
        }))
    }

    async fn tool_plan_trip(&self, arguments: Value) -> Result<Value> {
        #[derive(Debug, Deserialize)]
        struct Args {
            from_station: String,
            to_station: Option<String>,
            datetime: Option<String>,
            modes: Option<Vec<String>>,
            query: Option<String>,
        }

        let args: Args = serde_json::from_value(arguments)?;
        let (from_station, parsed_to_station) =
            split_route_phrase(&args.from_station, args.to_station.as_deref());
        let to_station = if parsed_to_station.is_empty() {
            return Err(anyhow!("destination station is required"));
        } else {
            parsed_to_station
        };

        let datetime = parse_datetime_input(args.datetime.as_deref())?;
        let modes = parse_modes(args.modes, args.query.as_deref());

        let (trips, warning) = self
            .ojp
            .plan_trip(&from_station, &to_station, datetime, &modes)
            .await?;

        let merged_warning = if trips.is_empty() {
            Some(ToolWarning {
                message:
                    "No connections found for the selected parameters. Try a broader mode filter or different departure time."
                        .to_string(),
                stale: false,
            })
        } else {
            warning
        };

        let response = TripsResponse {
            trips,
            warning: merged_warning,
        };

        Ok(json!(response))
    }

    async fn tool_get_departures(&self, arguments: Value) -> Result<Value> {
        #[derive(Debug, Deserialize)]
        struct Args {
            station_id: String,
            limit: Option<usize>,
            time_window: Option<i64>,
        }

        let args: Args = serde_json::from_value(arguments)?;
        let limit = args.limit.unwrap_or(20).min(100);
        let window = args.time_window.unwrap_or(120).max(1);

        let (mut departures, mut warning) =
            match self.ojp.departures(&args.station_id, limit, window).await {
                Ok(data) => data,
                Err(_) => {
                    self.gtfs_rt
                        .departures_for_station(&args.station_id, limit)
                        .await?
                }
            };

        if departures.iter().all(|dep| dep.delay_minutes == 0) {
            if let Ok((rt_departures, rt_warning)) = self
                .gtfs_rt
                .departures_for_station(&args.station_id, limit)
                .await
            {
                if !rt_departures.is_empty() {
                    departures = rt_departures;
                }
                warning = warning.or(rt_warning);
            }
        }

        departures.sort_by_key(|dep| dep.expected);
        departures.truncate(limit);

        let response = DeparturesResponse {
            departures,
            warning,
        };

        Ok(json!(response))
    }

    async fn tool_get_trip_details(&self, arguments: Value) -> Result<Value> {
        #[derive(Debug, Deserialize)]
        struct Args {
            trip_id: String,
        }

        let args: Args = serde_json::from_value(arguments)?;
        let (details, warning) = self.gtfs_rt.trip_details(&args.trip_id).await?;
        let details = details.ok_or_else(|| anyhow!("trip not found: {}", args.trip_id))?;

        let response = TripDetailsResponse {
            trip: details,
            warning,
        };

        Ok(json!(response))
    }

    async fn tool_monitor_trip(&self, arguments: Value) -> Result<Value> {
        #[derive(Debug, Deserialize)]
        struct Args {
            trip_id: String,
            notify_on_delay_minutes: Option<i32>,
        }

        let args: Args = serde_json::from_value(arguments)?;
        let threshold = args.notify_on_delay_minutes.unwrap_or(10).max(1);
        let subscription_id = uuid::Uuid::new_v4().to_string();

        let created_at = Utc::now().with_timezone(&Zurich).fixed_offset();
        let state = SubscriptionState {
            trip_id: args.trip_id.clone(),
            threshold_minutes: threshold,
            last_reported_delay_minutes: 0,
            created_at,
        };

        let mut subscriptions = self.subscriptions.write().await;
        subscriptions.insert(subscription_id.clone(), state.clone());

        let response = MonitorSubscription {
            subscription_id,
            trip_id: args.trip_id,
            notify_on_delay_minutes: threshold,
            created_at: state.created_at,
        };

        Ok(json!(response))
    }

    async fn tool_list_transport_modes(&self) -> Result<Value> {
        let modes = TransportMode::all_modes()
            .into_iter()
            .map(|mode| mode.as_str().to_string())
            .collect::<Vec<_>>();

        Ok(json!({ "modes": modes }))
    }

    async fn tool_get_disruptions(&self, arguments: Value) -> Result<Value> {
        #[derive(Debug, Deserialize)]
        struct Args {
            line: Option<String>,
            station: Option<String>,
        }

        let args: Args = serde_json::from_value(arguments).unwrap_or(Args {
            line: None,
            station: None,
        });

        let (disruptions, warning) = self
            .gtfs_rt
            .disruptions(args.line.as_deref(), args.station.as_deref())
            .await?;

        let response = DisruptionsResponse {
            disruptions,
            warning,
        };

        Ok(json!(response))
    }
}
