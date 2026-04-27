use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use chrono_tz::Europe::Zurich;
use prost::Message;
use reqwest::header::{ACCEPT, ACCEPT_ENCODING, USER_AGENT};
use tokio::sync::RwLock;

use crate::api::gtfs_realtime;
use crate::cache::TtlCache;
use crate::models::{
    Departure, Disruption, PlatformChange, StopUpdate, ToolWarning, TripDetails, TripStatus,
    format_service_label,
};

#[derive(Clone)]
pub struct GtfsRtClient {
    http: reqwest::Client,
    endpoint: String,
    token: String,
    cache: Arc<RwLock<TtlCache<String, gtfs_realtime::FeedMessage>>>,
}

impl GtfsRtClient {
    pub fn new(endpoint: String, token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoint,
            token,
            cache: Arc::new(RwLock::new(TtlCache::with_ttl(Duration::from_secs(30)))),
        }
    }

    async fn fetch_feed(&self) -> Result<(gtfs_realtime::FeedMessage, Option<ToolWarning>)> {
        let key = "feed".to_string();
        {
            let mut cache = self.cache.write().await;
            if let Some(feed) = cache.get(&key) {
                return Ok((feed, None));
            }
        }

        let response = self
            .http
            .get(&self.endpoint)
            .bearer_auth(&self.token)
            .header(
                USER_AGENT,
                concat!("swiss-transport-mcp/", env!("CARGO_PKG_VERSION")),
            )
            .header(ACCEPT, "application/octet-stream")
            .header(ACCEPT_ENCODING, "gzip, deflate, br")
            .send()
            .await;

        match response {
            Ok(response) => {
                if !response.status().is_success() {
                    let status = response.status();
                    let final_url = response.url().to_string();
                    let body = response.text().await.unwrap_or_default();
                    let body_excerpt = body.trim();
                    let body_detail = if body_excerpt.is_empty() {
                        String::new()
                    } else {
                        let truncated = body_excerpt.chars().take(220).collect::<String>();
                        format!(" (body: {truncated})")
                    };
                    return Err(anyhow!(
                        "GTFS-RT API returned status {} at {}{}",
                        status,
                        final_url,
                        body_detail
                    ));
                }
                let bytes = response.bytes().await?;
                let feed = gtfs_realtime::FeedMessage::decode(bytes)?;
                let mut cache = self.cache.write().await;
                cache.insert(key, feed.clone());
                Ok((feed, None))
            }
            Err(err) => {
                let cache = self.cache.read().await;
                if let Some(stale) = cache.get_stale(&key) {
                    Ok((
                        stale,
                        Some(ToolWarning {
                            message: format!(
                                "GTFS-RT request failed ({err}); returning stale cached feed"
                            ),
                            stale: true,
                        }),
                    ))
                } else {
                    Err(anyhow!(err))
                }
            }
        }
    }

    pub async fn departures_for_station(
        &self,
        station_id: &str,
        limit: usize,
    ) -> Result<(Vec<Departure>, Option<ToolWarning>)> {
        let (feed, warning) = self.fetch_feed().await?;
        let mut departures = Vec::new();

        for entity in feed.entity {
            let Some(update) = entity.trip_update else {
                continue;
            };
            let trip_id = update.trip.and_then(|trip| trip.trip_id);

            for stop_update in update.stop_time_update {
                if stop_update.stop_id.as_deref() != Some(station_id) {
                    continue;
                }

                let event = stop_update.departure.or(stop_update.arrival);
                let Some(event) = event else {
                    continue;
                };

                let expected = event
                    .time
                    .and_then(|timestamp| timestamp.try_into().ok())
                    .and_then(timestamp_to_fixed)
                    .unwrap_or_else(now_fixed);
                let delay = event.delay.unwrap_or(0);
                let scheduled = expected - chrono::Duration::minutes(delay as i64);

                departures.push(Departure {
                    trip_id: trip_id.clone(),
                    line: None,
                    service: format_service_label(None, None),
                    destination: stop_update
                        .stop_headsign
                        .clone()
                        .unwrap_or_else(|| "Unknown destination".to_string()),
                    mode: None,
                    scheduled,
                    expected,
                    delay_minutes: delay / 60,
                    platform: None,
                    status: TripStatus::from_delay(delay / 60),
                });
            }
        }

        departures.sort_by_key(|dep| dep.expected);
        departures.truncate(limit);
        Ok((departures, warning))
    }

    pub async fn trip_details(
        &self,
        trip_id: &str,
    ) -> Result<(Option<TripDetails>, Option<ToolWarning>)> {
        let (feed, warning) = self.fetch_feed().await?;

        for entity in feed.entity {
            let Some(update) = entity.trip_update else {
                continue;
            };
            let Some(trip) = update.trip.clone() else {
                continue;
            };
            let Some(candidate_trip_id) = trip.trip_id else {
                continue;
            };

            if candidate_trip_id != trip_id {
                continue;
            }

            let mut stop_updates = Vec::new();
            let mut max_delay = 0;

            for stop_update in update.stop_time_update {
                let arr_delay_secs = stop_update.arrival.and_then(|event| event.delay);
                let dep_delay_secs = stop_update.departure.and_then(|event| event.delay);
                let arr_delay_mins = arr_delay_secs.map(|seconds| seconds / 60);
                let dep_delay_mins = dep_delay_secs.map(|seconds| seconds / 60);

                if let Some(delay) = arr_delay_mins {
                    max_delay = max_delay.max(delay);
                }
                if let Some(delay) = dep_delay_mins {
                    max_delay = max_delay.max(delay);
                }

                stop_updates.push(StopUpdate {
                    stop_id: stop_update.stop_id.clone(),
                    stop_name: stop_update.stop_headsign.clone(),
                    arrival_delay_minutes: arr_delay_mins,
                    departure_delay_minutes: dep_delay_mins,
                    schedule_relationship: stop_update
                        .schedule_relationship
                        .and_then(|value| {
                            gtfs_realtime::stop_time_update::ScheduleRelationship::try_from(value)
                                .ok()
                        })
                        .map(|value| format!("{value:?}")),
                });
            }

            let is_cancelled = trip
                .schedule_relationship
                .and_then(|value| {
                    gtfs_realtime::trip_descriptor::ScheduleRelationship::try_from(value).ok()
                })
                .is_some_and(|status| {
                    status == gtfs_realtime::trip_descriptor::ScheduleRelationship::Canceled
                });

            return Ok((
                Some(TripDetails {
                    trip_id: trip_id.to_string(),
                    current_delay_minutes: max_delay,
                    is_cancelled,
                    platform_changes: Vec::<PlatformChange>::new(),
                    stop_updates,
                }),
                warning,
            ));
        }

        Ok((None, warning))
    }

    pub async fn disruptions(
        &self,
        line_filter: Option<&str>,
        station_filter: Option<&str>,
    ) -> Result<(Vec<Disruption>, Option<ToolWarning>)> {
        let (feed, warning) = self.fetch_feed().await?;
        let line_filter = line_filter.map(|value| value.to_ascii_lowercase());
        let station_filter = station_filter.map(|value| value.to_ascii_lowercase());
        let mut disruptions = Vec::new();

        for entity in feed.entity {
            let Some(alert) = entity.alert else {
                continue;
            };

            let mut affected_lines = Vec::new();
            let mut affected_stations = Vec::new();

            for informed in alert.informed_entity {
                if let Some(route_id) = informed.route_id {
                    affected_lines.push(route_id);
                }
                if let Some(stop_id) = informed.stop_id {
                    affected_stations.push(stop_id);
                }
            }

            if let Some(filter) = line_filter.as_ref() {
                if !affected_lines
                    .iter()
                    .any(|line| line.to_ascii_lowercase().contains(filter))
                {
                    continue;
                }
            }

            if let Some(filter) = station_filter.as_ref() {
                if !affected_stations
                    .iter()
                    .any(|stop| stop.to_ascii_lowercase().contains(filter))
                {
                    continue;
                }
            }

            let active_period = alert.active_period.first();
            disruptions.push(Disruption {
                id: entity
                    .id
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                affected_lines,
                affected_stations,
                reason: alert
                    .cause
                    .and_then(|value| gtfs_realtime::alert::Cause::try_from(value).ok())
                    .map(|value| format!("{value:?}")),
                description: translated_text(alert.description_text.as_ref())
                    .or_else(|| translated_text(alert.header_text.as_ref())),
                starts_at: active_period
                    .and_then(|range| range.start)
                    .and_then(timestamp_to_fixed),
                ends_at: active_period
                    .and_then(|range| range.end)
                    .and_then(timestamp_to_fixed),
                alternatives: Vec::new(),
            });
        }

        Ok((disruptions, warning))
    }
}

fn translated_text(value: Option<&gtfs_realtime::TranslatedString>) -> Option<String> {
    value.and_then(|text| {
        text.translation
            .iter()
            .find_map(|entry| entry.text.clone())
            .or_else(|| {
                text.translation
                    .first()
                    .and_then(|entry| entry.text.clone())
            })
    })
}

fn timestamp_to_fixed(timestamp: u64) -> Option<DateTime<FixedOffset>> {
    let datetime = Utc.timestamp_opt(timestamp as i64, 0).single()?;
    Some(datetime.with_timezone(&Zurich).fixed_offset())
}

fn now_fixed() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&Zurich).fixed_offset()
}
