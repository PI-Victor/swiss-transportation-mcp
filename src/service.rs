use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use chrono::{DateTime, FixedOffset, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Europe::Zurich;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::RwLock;

use crate::api::formation::{FormationClient, render_formation_short_string};
use crate::api::gtfs_rt::GtfsRtClient;
use crate::api::ojp::OjpClient;
use crate::config::Config;
use crate::models::{
    DeparturesResponse, Disruption, DisruptionsResponse, GeoPoint, MonitorSubscription, Station,
    StationsResponse, ToolWarning, TransportMode, Trip, TripDetailsResponse, TripOptionPlatform,
    TripOptionReport, TripOptionTime, TripPlanReport, TripStatus, TripsResponse,
};

#[derive(Debug, Clone)]
struct SubscriptionState {
    trip_id: String,
    threshold_minutes: i32,
    last_reported_delay_minutes: i32,
    created_at: chrono::DateTime<FixedOffset>,
}

#[derive(Debug, Clone)]
struct ResolvedEndpoint {
    id: String,
    name: String,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub ojp: OjpClient,
    pub gtfs_rt: GtfsRtClient,
    pub formation: FormationClient,
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
        let formation = FormationClient::new(
            config.formation_endpoint.clone(),
            config.formation_token.clone(),
        );

        Self {
            config,
            ojp,
            gtfs_rt,
            formation,
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value> {
        match tool {
            "search_stations" => self.tool_search_stations(arguments).await,
            "resolve_place" => self.tool_resolve_place(arguments).await,
            "plan_trip" => self.tool_plan_trip(arguments).await,
            "plan_trip_point_to_point" => self.tool_plan_trip_point_to_point(arguments).await,
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

    async fn tool_resolve_place(&self, arguments: Value) -> Result<Value> {
        #[derive(Debug, Deserialize)]
        struct Args {
            query: String,
            limit: Option<usize>,
            strict_exact: Option<bool>,
        }

        let args: Args = serde_json::from_value(arguments)?;
        let query = args.query.trim();
        if query.is_empty() {
            return Err(anyhow!("query is required"));
        }

        let limit = args.limit.unwrap_or(8).clamp(1, 20);
        let strict_exact = args.strict_exact.unwrap_or(false);

        let (stations, suggestions, warning) = self.ojp.search_stations(query, limit).await?;
        if stations.is_empty() {
            return Err(anyhow!(
                "place not found: {} (suggestions: {})",
                query,
                suggestions.join(", ")
            ));
        }

        let resolved =
            select_station_candidate(query, &stations, strict_exact).ok_or_else(|| {
                anyhow!(
                    "no exact match for '{}'; candidates: {}",
                    query,
                    format_station_candidates(&stations)
                )
            })?;

        Ok(json!({
            "result": {
                "resolved": resolved,
                "candidates": stations,
                "suggestions": suggestions,
            },
            "warning": warning,
        }))
    }

    async fn tool_plan_trip(&self, arguments: Value) -> Result<Value> {
        #[derive(Debug, Deserialize)]
        struct Args {
            from_station: Option<String>,
            from_station_id: Option<String>,
            to_station: Option<String>,
            to_station_id: Option<String>,
            datetime: Option<String>,
            modes: Option<Vec<String>>,
            limit: Option<usize>,
            strict_resolution: Option<bool>,
        }

        let args: Args = serde_json::from_value(arguments)?;
        let datetime = parse_datetime_value(args.datetime.as_deref())?;
        let modes = parse_structured_modes(args.modes);
        let max_options = args.limit.unwrap_or(5).clamp(1, 5);
        let strict = args.strict_resolution.unwrap_or(false);
        let origin = self
            .resolve_trip_endpoint(
                "origin",
                args.from_station.as_deref(),
                args.from_station_id.as_deref(),
                strict,
            )
            .await?;
        let destination = self
            .resolve_trip_endpoint(
                "destination",
                args.to_station.as_deref(),
                args.to_station_id.as_deref(),
                strict,
            )
            .await?;

        self.plan_trip_with_resolved_endpoints(origin, destination, datetime, &modes, max_options)
            .await
    }

    async fn tool_plan_trip_point_to_point(&self, arguments: Value) -> Result<Value> {
        #[derive(Debug, Deserialize)]
        struct Args {
            from_id: String,
            to_id: String,
            from_name: Option<String>,
            to_name: Option<String>,
            datetime: Option<String>,
            modes: Option<Vec<String>>,
            limit: Option<usize>,
        }

        let args: Args = serde_json::from_value(arguments)?;
        let from_id = args.from_id.trim();
        let to_id = args.to_id.trim();
        if from_id.is_empty() || to_id.is_empty() {
            return Err(anyhow!("from_id and to_id are required"));
        }

        let datetime = parse_datetime_value(args.datetime.as_deref())?;
        let modes = parse_structured_modes(args.modes);
        let max_options = args.limit.unwrap_or(5).clamp(1, 5);

        let origin = ResolvedEndpoint {
            id: from_id.to_string(),
            name: normalize_optional_text(args.from_name).unwrap_or_else(|| from_id.to_string()),
        };
        let destination = ResolvedEndpoint {
            id: to_id.to_string(),
            name: normalize_optional_text(args.to_name).unwrap_or_else(|| to_id.to_string()),
        };

        self.plan_trip_with_resolved_endpoints(origin, destination, datetime, &modes, max_options)
            .await
    }

    async fn resolve_trip_endpoint(
        &self,
        role: &str,
        station_name: Option<&str>,
        station_id: Option<&str>,
        strict: bool,
    ) -> Result<ResolvedEndpoint> {
        if let Some(id) = station_id.map(str::trim).filter(|value| !value.is_empty()) {
            let display_name =
                station_name.and_then(|name| normalize_optional_text(Some(name.to_string())));
            return Ok(ResolvedEndpoint {
                id: id.to_string(),
                name: display_name.unwrap_or_else(|| id.to_string()),
            });
        }

        let query = station_name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("{role} station is required"))?;

        let (stations, suggestions, _) = self.ojp.search_stations(query, 12).await?;
        if stations.is_empty() {
            return Err(anyhow!(
                "{} station not found: {} (suggestions: {})",
                role,
                query,
                suggestions.join(", ")
            ));
        }

        let selected = select_station_candidate(query, &stations, strict).ok_or_else(|| {
            anyhow!(
                "no exact {} station match for '{}'; candidates: {}",
                role,
                query,
                format_station_candidates(&stations)
            )
        })?;

        Ok(ResolvedEndpoint {
            id: selected.id.clone(),
            name: selected.name.clone(),
        })
    }

    async fn plan_trip_with_resolved_endpoints(
        &self,
        origin: ResolvedEndpoint,
        destination: ResolvedEndpoint,
        datetime: DateTime<FixedOffset>,
        modes: &[TransportMode],
        max_options: usize,
    ) -> Result<Value> {
        let (trips, warning) = self
            .ojp
            .plan_trip_with_refs(
                &origin.name,
                &origin.id,
                &destination.name,
                &destination.id,
                datetime,
                modes,
                max_options,
            )
            .await?;

        let mut limited_trips = trips;
        if limited_trips.len() > max_options {
            limited_trips.truncate(max_options);
        }
        self.enrich_trip_coordinates(&mut limited_trips).await;

        let merged_warning = if limited_trips.is_empty() {
            Some(ToolWarning {
                message:
                    "No connections found for the selected parameters. Try a broader mode filter or different departure time."
                        .to_string(),
                stale: false,
            })
        } else {
            warning
        };

        let report = self
            .build_trip_report(&origin.name, &destination.name, &limited_trips)
            .await
            .ok();
        let option_reports = self
            .build_option_reports(
                &limited_trips,
                &origin.name,
                &destination.name,
                report.as_ref(),
            )
            .await;

        let response = TripsResponse {
            trips: limited_trips,
            warning: merged_warning,
            report,
            option_reports,
        };

        let mut payload = serde_json::to_value(response)?;
        if let Value::Object(map) = &mut payload {
            map.insert(
                "resolvedFrom".to_string(),
                json!({
                    "id": origin.id,
                    "name": origin.name,
                }),
            );
            map.insert(
                "resolvedTo".to_string(),
                json!({
                    "id": destination.id,
                    "name": destination.name,
                }),
            );
        }

        Ok(payload)
    }

    async fn tool_get_departures(&self, arguments: Value) -> Result<Value> {
        #[derive(Debug, Deserialize)]
        struct Args {
            station_id: Option<String>,
            station_query: Option<String>,
            limit: Option<usize>,
            time_window: Option<i64>,
            modes: Option<Vec<String>>,
            query: Option<String>,
        }

        let args: Args = serde_json::from_value(arguments)?;
        let limit = args.limit.unwrap_or(20).min(100);
        let window = args.time_window.unwrap_or(120).max(1);
        let modes = parse_structured_modes(args.modes);
        let _ = args.query;
        let station_id = match args
            .station_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(id) => id.to_string(),
            None => {
                let query = args
                    .station_query
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow!("station_id or station_query is required"))?;
                let (stations, suggestions, _) = self.ojp.search_stations(query, 10).await?;
                let resolved = stations
                    .first()
                    .map(|station| station.id.clone())
                    .ok_or_else(|| {
                        anyhow!(
                            "station not found: {} (suggestions: {})",
                            query,
                            suggestions.join(", ")
                        )
                    })?;
                resolved
            }
        };

        let (mut departures, mut warning) =
            match self.ojp.departures(&station_id, limit, window).await {
                Ok(data) => data,
                Err(_) => {
                    self.gtfs_rt
                        .departures_for_station(&station_id, limit)
                        .await?
                }
            };

        if departures.iter().all(|dep| dep.delay_minutes == 0) {
            if let Ok((rt_departures, rt_warning)) = self
                .gtfs_rt
                .departures_for_station(&station_id, limit)
                .await
            {
                if !rt_departures.is_empty() {
                    departures = rt_departures;
                }
                warning = warning.or(rt_warning);
            }
        }

        if !modes.iter().any(|mode| matches!(mode, TransportMode::All)) {
            departures.retain(|departure| {
                let mode = departure
                    .mode
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                modes.iter().any(|candidate| match candidate {
                    TransportMode::Ship => matches!(mode.as_str(), "ship" | "boat" | "water"),
                    _ => mode == candidate.as_str(),
                })
            });
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

        let (disruptions, warning) = match self
            .gtfs_rt
            .disruptions(args.line.as_deref(), args.station.as_deref())
            .await
        {
            Ok(payload) => payload,
            Err(err) => {
                return Err(anyhow!("{}", self.live_events_error_message(&err)));
            }
        };

        let response = DisruptionsResponse {
            disruptions,
            warning,
        };

        Ok(json!(response))
    }

    async fn build_trip_report(
        &self,
        origin: &str,
        destination: &str,
        trips: &[Trip],
    ) -> Result<TripPlanReport> {
        let delayed_options = trips
            .iter()
            .filter(|trip| {
                trip.legs.iter().any(|leg| {
                    matches!(
                        leg.status,
                        TripStatus::Delayed
                            | TripStatus::Cancelled
                            | TripStatus::Redirected
                            | TripStatus::PlatformChanged
                    )
                })
            })
            .count();

        let (live_events, live_events_warning) = match self.gtfs_rt.disruptions(None, None).await {
            Ok((disruptions, warning)) => {
                let relevant =
                    filter_relevant_disruptions(&disruptions, trips, origin, destination, true);
                (relevant, warning)
            }
            Err(err) => (
                Vec::new(),
                Some(ToolWarning {
                    message: self.live_events_error_message(&err),
                    stale: false,
                }),
            ),
        };
        let live_events_status = if let Some(warning) = live_events_warning.as_ref() {
            format!("unavailable: {}", warning.message)
        } else if live_events.is_empty() {
            "none".to_string()
        } else {
            "available".to_string()
        };

        Ok(TripPlanReport {
            generated_at: Utc::now().with_timezone(&Zurich).fixed_offset(),
            origin: origin.to_string(),
            destination: destination.to_string(),
            total_options: trips.len(),
            delayed_options,
            live_events,
            live_events_status,
            live_events_warning,
        })
    }

    async fn build_option_reports(
        &self,
        trips: &[Trip],
        origin: &str,
        destination: &str,
        report: Option<&TripPlanReport>,
    ) -> Vec<TripOptionReport> {
        let base_events = report
            .map(|entry| entry.live_events.clone())
            .unwrap_or_default();
        let warning_message = report
            .and_then(|entry| entry.live_events_warning.as_ref())
            .map(|warning| warning.message.clone());

        let mut reports = Vec::with_capacity(trips.len());
        for trip in trips {
            let first_leg = trip.legs.first();
            let last_leg = trip.legs.last();

            let departure = first_leg
                .map(|leg| leg.departure.expected)
                .unwrap_or_else(|| Utc::now().with_timezone(&Zurich).fixed_offset());
            let arrival = last_leg
                .map(|leg| leg.arrival.expected)
                .unwrap_or(departure);

            let departure_platform =
                first_leg
                    .and_then(|leg| leg.from.platform.clone())
                    .or_else(|| {
                        first_leg
                            .and_then(|leg| leg.stop_calls.first())
                            .and_then(|call| call.platform.clone())
                    });
            let arrival_platform = last_leg
                .and_then(|leg| leg.to.platform.clone())
                .or_else(|| {
                    last_leg
                        .and_then(|leg| leg.stop_calls.last())
                        .and_then(|call| call.platform.clone())
                });
            let final_destination = last_leg
                .map(|leg| leg.to.station.clone())
                .unwrap_or_else(|| destination.to_string());

            let events = if warning_message.is_some() || base_events.is_empty() {
                Vec::new()
            } else {
                filter_relevant_disruptions(
                    &base_events,
                    std::slice::from_ref(trip),
                    origin,
                    destination,
                    false,
                )
            };

            let events_status = if let Some(message) = warning_message.as_ref() {
                format!("unavailable: {message}")
            } else if events.is_empty() {
                "none".to_string()
            } else {
                "available".to_string()
            };

            let origin_coordinates = first_leg
                .and_then(|leg| leg.from.coordinates.clone())
                .or_else(|| {
                    first_leg
                        .and_then(|leg| leg.stop_calls.first())
                        .and_then(|call| call.coordinates.clone())
                });
            let final_destination_coordinates = last_leg
                .and_then(|leg| leg.to.coordinates.clone())
                .or_else(|| {
                    last_leg
                        .and_then(|leg| leg.stop_calls.last())
                        .and_then(|call| call.coordinates.clone())
                });
            let occupancy = trip.legs.iter().find_map(|leg| leg.occupancy.clone());
            let (wagon_formation, wagon_formation_status) =
                match self.formation.wagon_formation_for_trip(trip).await {
                    Ok(Some(value)) => (Some(value), "available".to_string()),
                    Ok(None) => (None, "none".to_string()),
                    Err(err) => (None, format!("unavailable: {err}")),
                };
            let (
                wagon_formation_readable,
                wagon_formation_diagram,
                wagon_formation_human_display,
                wagon_formation_boarding_hint,
                wagon_formation_legend,
                wagon_formation_parsed,
                sector_to_coach_range,
                accessible_coaches,
                first_class_coaches,
                second_class_coaches,
            ) = wagon_formation
                .as_deref()
                .and_then(render_formation_short_string)
                .map(|rendered| {
                    (
                        Some(rendered.readable),
                        Some(rendered.diagram),
                        Some(rendered.human_display),
                        Some(rendered.boarding_hint),
                        Some(rendered.legend),
                        Some(rendered.parsed),
                        Some(rendered.sector_to_coach_range),
                        Some(rendered.accessible_coaches),
                        Some(rendered.first_class_coaches),
                        Some(rendered.second_class_coaches),
                    )
                })
                .unwrap_or((None, None, None, None, None, None, None, None, None, None));

            reports.push(TripOptionReport {
                trip_id: trip.id.clone(),
                time: TripOptionTime {
                    departure,
                    arrival,
                    duration_minutes: (arrival - departure).num_minutes().max(0),
                },
                platform: TripOptionPlatform {
                    departure: departure_platform,
                    arrival: arrival_platform,
                },
                final_destination,
                origin_coordinates,
                final_destination_coordinates,
                occupancy,
                events,
                events_status,
                wagon_formation,
                wagon_formation_status,
                wagon_formation_readable,
                wagon_formation_diagram,
                wagon_formation_human_display,
                wagon_formation_boarding_hint,
                wagon_formation_legend,
                wagon_formation_parsed,
                sector_to_coach_range,
                accessible_coaches,
                first_class_coaches,
                second_class_coaches,
            });
        }

        reports
    }

    async fn enrich_trip_coordinates(&self, trips: &mut [Trip]) {
        if trips.is_empty() {
            return;
        }

        let mut station_names = HashSet::new();
        for trip in trips.iter() {
            for leg in &trip.legs {
                station_names.insert(leg.from.station.clone());
                station_names.insert(leg.to.station.clone());
                for call in &leg.stop_calls {
                    station_names.insert(call.station.clone());
                }
            }
        }

        let mut coordinates_by_station = HashMap::new();
        for station_name in station_names {
            if let Ok((stations, _, _)) = self.ojp.search_stations(&station_name, 8).await
                && let Some(point) = pick_station_coordinates(&station_name, &stations)
            {
                coordinates_by_station.insert(station_name.to_ascii_lowercase(), point);
            }
        }

        for trip in trips.iter_mut() {
            for leg in &mut trip.legs {
                if leg.from.coordinates.is_none() {
                    leg.from.coordinates = coordinates_by_station
                        .get(&leg.from.station.to_ascii_lowercase())
                        .cloned();
                }
                if leg.to.coordinates.is_none() {
                    leg.to.coordinates = coordinates_by_station
                        .get(&leg.to.station.to_ascii_lowercase())
                        .cloned();
                }
                for call in &mut leg.stop_calls {
                    if call.coordinates.is_none() {
                        call.coordinates = coordinates_by_station
                            .get(&call.station.to_ascii_lowercase())
                            .cloned();
                    }
                }
            }
        }
    }

    fn live_events_error_message(&self, err: &anyhow::Error) -> String {
        let base = format!("live event feed unavailable ({err})");
        if !is_gtfs_auth_failure(err) {
            return base;
        }

        format!(
            "{base}. GTFS auth diagnostics: endpoint={}, token_source=dedicated GTFS_RT_TOKEN, verify GTFS_RT_TOKEN is correctly propagated in mcp_servers.<id>.env_vars and matches the token that succeeds in shell probes",
            self.config.gtfs_rt_endpoint
        )
    }
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn select_station_candidate<'a>(
    query: &str,
    stations: &'a [Station],
    strict_exact: bool,
) -> Option<&'a Station> {
    if stations.is_empty() {
        return None;
    }

    let normalized_query = query.trim().to_ascii_lowercase();
    let exact = stations
        .iter()
        .find(|station| station.name.trim().to_ascii_lowercase() == normalized_query);
    if exact.is_some() {
        return exact;
    }

    if strict_exact {
        return None;
    }

    stations
        .iter()
        .find(|station| {
            station
                .name
                .trim()
                .to_ascii_lowercase()
                .starts_with(&normalized_query)
        })
        .or_else(|| stations.first())
}

fn format_station_candidates(stations: &[Station]) -> String {
    stations
        .iter()
        .take(8)
        .map(|station| format!("{} ({})", station.name, station.id))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_datetime_value(input: Option<&str>) -> Result<DateTime<FixedOffset>> {
    let now = Utc::now().with_timezone(&Zurich);
    let Some(value) = input.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(now.fixed_offset());
    };

    let lower = value.to_ascii_lowercase();
    if lower == "earliest" || lower == "now" {
        return Ok(now.fixed_offset());
    }

    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed.with_timezone(&Zurich).fixed_offset());
    }

    if let Ok(parsed) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M") {
        return map_zurich_naive(parsed);
    }

    if let Ok(parsed) = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M") {
        return map_zurich_naive(parsed);
    }

    if lower.contains("tomorrow") {
        let tomorrow = now
            .date_naive()
            .succ_opt()
            .ok_or_else(|| anyhow!("failed to compute tomorrow date"))?;
        let parsed_time = parse_hour_minute(&lower)
            .unwrap_or_else(|| NaiveTime::from_hms_opt(6, 0, 0).unwrap_or(NaiveTime::MIN));
        return map_zurich_naive(tomorrow.and_time(parsed_time));
    }

    Err(anyhow!("unsupported datetime value: {value}"))
}

fn map_zurich_naive(value: NaiveDateTime) -> Result<DateTime<FixedOffset>> {
    match Zurich.from_local_datetime(&value) {
        LocalResult::Single(datetime) => Ok(datetime.fixed_offset()),
        LocalResult::Ambiguous(first, _) => Ok(first.fixed_offset()),
        LocalResult::None => Err(anyhow!(
            "datetime is invalid in Europe/Zurich timezone: {}",
            value
        )),
    }
}

fn parse_hour_minute(text: &str) -> Option<NaiveTime> {
    let token = text
        .split_whitespace()
        .find(|candidate| candidate.chars().any(|ch| ch.is_ascii_digit()))?;
    let mut parts = token.split(':');
    let hour = parts.next()?.parse::<u32>().ok()?.min(23);
    let minute = parts
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0)
        .min(59);
    NaiveTime::from_hms_opt(hour, minute, 0)
}

fn parse_structured_modes(modes: Option<Vec<String>>) -> Vec<TransportMode> {
    let Some(modes) = modes else {
        return vec![TransportMode::All];
    };

    let mut parsed = Vec::new();
    for mode in modes {
        if let Some(mode) = parse_mode_token(&mode)
            && !parsed
                .iter()
                .any(|existing: &TransportMode| existing.as_str() == mode.as_str())
        {
            parsed.push(mode);
        }
    }

    if parsed.is_empty() {
        vec![TransportMode::All]
    } else {
        parsed
    }
}

fn parse_mode_token(token: &str) -> Option<TransportMode> {
    match token.trim().to_ascii_lowercase().as_str() {
        "rail" | "train" => Some(TransportMode::Rail),
        "bus" => Some(TransportMode::Bus),
        "tram" => Some(TransportMode::Tram),
        "ship" | "boat" | "water" => Some(TransportMode::Ship),
        "cableway" | "cable" => Some(TransportMode::Cableway),
        "funicular" | "standseilbahn" => Some(TransportMode::Funicular),
        "all" => Some(TransportMode::All),
        _ => None,
    }
}

fn is_gtfs_auth_failure(err: &anyhow::Error) -> bool {
    let message = err.to_string().to_ascii_lowercase();
    message.contains("401")
        || message.contains("403")
        || message.contains("unauthorized")
        || message.contains("forbidden")
}

fn pick_station_coordinates(name: &str, stations: &[crate::models::Station]) -> Option<GeoPoint> {
    let normalized = name.trim().to_ascii_lowercase();
    if let Some(exact) = stations.iter().find(|station| {
        station.name.trim().to_ascii_lowercase() == normalized
            && station.latitude.is_some()
            && station.longitude.is_some()
    }) {
        return Some(GeoPoint {
            latitude: exact.latitude?,
            longitude: exact.longitude?,
        });
    }

    stations.iter().find_map(|station| {
        Some(GeoPoint {
            latitude: station.latitude?,
            longitude: station.longitude?,
        })
    })
}

fn filter_relevant_disruptions(
    disruptions: &[Disruption],
    trips: &[Trip],
    origin: &str,
    destination: &str,
    include_fallback: bool,
) -> Vec<Disruption> {
    if disruptions.is_empty() {
        return Vec::new();
    }

    let lines = trips
        .iter()
        .flat_map(|trip| trip.legs.iter())
        .filter_map(|leg| leg.line.as_deref())
        .map(|line| line.to_ascii_lowercase())
        .collect::<Vec<_>>();

    let stations = collect_station_tokens(trips, origin, destination);

    let mut relevant = disruptions
        .iter()
        .filter(|disruption| {
            let by_line = disruption.affected_lines.iter().any(|line| {
                let line_lower = line.to_ascii_lowercase();
                lines
                    .iter()
                    .any(|candidate| !candidate.is_empty() && line_lower.contains(candidate))
            });
            if by_line {
                return true;
            }

            let text = disruption.description.clone().unwrap_or_default();
            let text_lower = text.to_ascii_lowercase();
            stations
                .iter()
                .any(|token| !token.is_empty() && text_lower.contains(token))
        })
        .cloned()
        .collect::<Vec<_>>();

    if include_fallback && relevant.is_empty() {
        relevant = disruptions.iter().take(8).cloned().collect();
    }

    relevant
}

fn collect_station_tokens(trips: &[Trip], origin: &str, destination: &str) -> Vec<String> {
    let mut stations = vec![
        origin.to_ascii_lowercase(),
        destination.to_ascii_lowercase(),
    ];
    stations.extend(trips.iter().flat_map(|trip| {
        trip.legs.iter().flat_map(|leg| {
            let mut names = Vec::new();
            names.push(leg.from.station.to_ascii_lowercase());
            names.push(leg.to.station.to_ascii_lowercase());
            names.extend(leg.stops.iter().map(|stop| stop.to_ascii_lowercase()));
            names
        })
    }));

    stations.sort();
    stations.dedup();
    stations
}

#[cfg(test)]
mod tests {
    use super::{normalize_optional_text, select_station_candidate};
    use crate::models::{Station, StationType};

    fn station(id: &str, name: &str) -> Station {
        Station {
            id: id.to_string(),
            name: name.to_string(),
            latitude: None,
            longitude: None,
            station_type: StationType::Stop,
        }
    }

    #[test]
    fn select_station_candidate_prefers_exact_match() {
        let stations = vec![
            station("1", "Luzern Bahnhof"),
            station("2", "Luzern Allmend/Messe"),
        ];
        let selected = select_station_candidate("Luzern Bahnhof", &stations, false)
            .expect("candidate should be selected");
        assert_eq!(selected.id, "1");
    }

    #[test]
    fn select_station_candidate_strict_requires_exact_match() {
        let stations = vec![
            station("1", "Luzern Bahnhof"),
            station("2", "Luzern Allmend/Messe"),
        ];
        let selected = select_station_candidate("Luzern", &stations, true);
        assert!(selected.is_none());
    }

    #[test]
    fn normalize_optional_text_trims_and_rejects_empty() {
        assert_eq!(
            normalize_optional_text(Some("  Kriens Zentrum Pilatus ".to_string())),
            Some("Kriens Zentrum Pilatus".to_string())
        );
        assert_eq!(normalize_optional_text(Some("   ".to_string())), None);
        assert_eq!(normalize_optional_text(None), None);
    }
}
