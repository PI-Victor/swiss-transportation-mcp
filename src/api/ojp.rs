use std::collections::HashSet;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use chrono_tz::Europe::Zurich;
use quick_xml::Reader;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::name::QName;
use quick_xml::writer::Writer;
use tokio::sync::RwLock;

use crate::cache::TtlCache;
use crate::models::{
    Departure, LegStop, Station, StationType, TimeInfo, ToolWarning, TransportMode, Trip, TripLeg,
    TripStatus,
};

#[derive(Default, Clone)]
struct TempLeg {
    mode: Option<String>,
    line: Option<String>,
    operator: Option<String>,
    from_station: Option<String>,
    from_platform: Option<String>,
    to_station: Option<String>,
    to_platform: Option<String>,
    departure_scheduled: Option<DateTime<FixedOffset>>,
    departure_expected: Option<DateTime<FixedOffset>>,
    arrival_scheduled: Option<DateTime<FixedOffset>>,
    arrival_expected: Option<DateTime<FixedOffset>>,
}

#[derive(Clone)]
pub struct OjpClient {
    http: reqwest::Client,
    endpoint: String,
    token: String,
    station_cache: Arc<RwLock<TtlCache<String, Vec<Station>>>>,
    trip_cache: Arc<RwLock<TtlCache<String, Vec<Trip>>>>,
    known_stations: Arc<RwLock<Vec<Station>>>,
}

impl OjpClient {
    pub fn new(endpoint: String, token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoint,
            token,
            station_cache: Arc::new(RwLock::new(TtlCache::with_ttl(Duration::from_secs(
                24 * 60 * 60,
            )))),
            trip_cache: Arc::new(RwLock::new(TtlCache::with_ttl(Duration::from_secs(120)))),
            known_stations: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn search_stations(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<(Vec<Station>, Vec<String>, Option<ToolWarning>)> {
        let cache_key = format!("{}:{}", query.to_ascii_lowercase(), limit);
        {
            let mut cache = self.station_cache.write().await;
            if let Some(stations) = cache.get(&cache_key) {
                let suggestions = self.suggest_station_names(query, &stations).await;
                return Ok((stations, suggestions, None));
            }
        }

        let xml = build_location_information_request(query, limit);
        let response = self.send_ojp_request(xml).await;

        match response {
            Ok(xml_response) => {
                let stations = parse_station_response(&xml_response, limit)?;
                {
                    let mut cache = self.station_cache.write().await;
                    cache.insert(cache_key, stations.clone());
                }
                {
                    let mut known = self.known_stations.write().await;
                    for station in &stations {
                        if !known.iter().any(|existing| existing.id == station.id) {
                            known.push(station.clone());
                        }
                    }
                }
                let suggestions = self.suggest_station_names(query, &stations).await;
                Ok((stations, suggestions, None))
            }
            Err(err) => {
                let cache = self.station_cache.read().await;
                if let Some(stale) = cache.get_stale(&cache_key) {
                    let suggestions = self.suggest_station_names(query, &stale).await;
                    Ok((
                        stale,
                        suggestions,
                        Some(ToolWarning {
                            message: format!(
                                "OJP station search failed ({err}); returning stale cached data"
                            ),
                            stale: true,
                        }),
                    ))
                } else {
                    Err(err)
                }
            }
        }
    }

    pub async fn plan_trip(
        &self,
        from_station: &str,
        to_station: &str,
        datetime: DateTime<FixedOffset>,
        modes: &[TransportMode],
    ) -> Result<(Vec<Trip>, Option<ToolWarning>)> {
        let mode_key = modes
            .iter()
            .map(TransportMode::as_str)
            .collect::<Vec<_>>()
            .join(",");
        let cache_key = format!(
            "{}:{}:{}:{}",
            from_station.to_ascii_lowercase(),
            to_station.to_ascii_lowercase(),
            datetime.to_rfc3339(),
            mode_key
        );

        {
            let mut cache = self.trip_cache.write().await;
            if let Some(trips) = cache.get(&cache_key) {
                return Ok((trips, None));
            }
        }

        let xml = build_trip_request(from_station, to_station, datetime, modes);
        let response = self.send_ojp_request(xml).await;

        match response {
            Ok(xml_response) => {
                let trips = parse_trip_response(&xml_response)?;
                let mut cache = self.trip_cache.write().await;
                cache.insert(cache_key, trips.clone());
                Ok((trips, None))
            }
            Err(err) => {
                let cache = self.trip_cache.read().await;
                if let Some(stale) = cache.get_stale(&cache_key) {
                    Ok((
                        stale,
                        Some(ToolWarning {
                            message: format!(
                                "OJP trip planning failed ({err}); returning stale cached data"
                            ),
                            stale: true,
                        }),
                    ))
                } else {
                    Err(err)
                }
            }
        }
    }

    pub async fn departures(
        &self,
        station_id: &str,
        limit: usize,
        window_minutes: i64,
    ) -> Result<(Vec<Departure>, Option<ToolWarning>)> {
        let xml = build_stop_event_request(station_id, limit, window_minutes);
        let response = self.send_ojp_request(xml).await;

        match response {
            Ok(xml_response) => {
                parse_departure_response(&xml_response, limit).map(|deps| (deps, None))
            }
            Err(err) => Err(err),
        }
    }

    async fn send_ojp_request(&self, body: String) -> Result<String> {
        let mut attempts = 0usize;
        let mut delay_ms = 200u64;

        loop {
            let response = self
                .http
                .post(&self.endpoint)
                .bearer_auth(&self.token)
                .header("Content-Type", "application/xml")
                .body(body.clone())
                .send()
                .await;

            match response {
                Ok(response) => {
                    if response.status().is_success() {
                        return Ok(response.text().await?);
                    }

                    let status = response.status();
                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempts < 4 {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        attempts += 1;
                        delay_ms *= 2;
                        continue;
                    }

                    return Err(anyhow!("OJP API returned status {}", status));
                }
                Err(err) => {
                    if attempts < 4 {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        attempts += 1;
                        delay_ms *= 2;
                        continue;
                    }
                    return Err(anyhow!(err));
                }
            }
        }
    }

    async fn suggest_station_names(&self, query: &str, local_results: &[Station]) -> Vec<String> {
        let mut candidates = local_results
            .iter()
            .map(|station| station.name.clone())
            .collect::<Vec<_>>();

        let known = self.known_stations.read().await;
        candidates.extend(known.iter().map(|station| station.name.clone()));
        candidates.sort();
        candidates.dedup();

        let mut ranked = candidates
            .into_iter()
            .map(|name| {
                let score =
                    strsim::jaro_winkler(&name.to_ascii_lowercase(), &query.to_ascii_lowercase());
                (name, score)
            })
            .collect::<Vec<_>>();

        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
            .into_iter()
            .filter(|(_, score)| *score >= 0.70)
            .take(5)
            .map(|(name, _)| name)
            .collect()
    }
}

fn build_location_information_request(query: &str, limit: usize) -> String {
    let timestamp = Utc::now().with_timezone(&Zurich).to_rfc3339();
    let limit_value = limit.to_string();
    let mut writer = Writer::new(Vec::<u8>::new());

    write_ojp_root_start(&mut writer);

    write_start(&mut writer, "OJPRequest");
    write_start(&mut writer, "siri:ServiceRequest");
    write_text_element(&mut writer, "siri:RequestTimestamp", &timestamp);
    write_text_element(&mut writer, "siri:RequestorRef", "sbb-mcp");
    write_start(&mut writer, "OJPLocationInformationRequest");
    write_text_element(&mut writer, "RequestTimestamp", &timestamp);
    write_start(&mut writer, "Location");
    write_text_element(&mut writer, "LocationName", query);
    write_end(&mut writer, "Location");
    write_start(&mut writer, "Params");
    write_text_element(&mut writer, "NumberOfResults", &limit_value);
    write_text_element(&mut writer, "IncludePtModes", "true");
    write_end(&mut writer, "Params");
    write_end(&mut writer, "OJPLocationInformationRequest");
    write_end(&mut writer, "siri:ServiceRequest");
    write_end(&mut writer, "OJPRequest");
    write_end(&mut writer, "OJP");

    String::from_utf8(writer.into_inner())
        .expect("OJP location request XML should always be valid UTF-8")
}

fn build_trip_request(
    from_station: &str,
    to_station: &str,
    datetime: DateTime<FixedOffset>,
    modes: &[TransportMode],
) -> String {
    let timestamp = Utc::now().with_timezone(&Zurich).to_rfc3339();
    let departure_time = datetime.to_rfc3339();
    let mut writer = Writer::new(Vec::<u8>::new());

    write_ojp_root_start(&mut writer);
    write_start(&mut writer, "OJPRequest");
    write_start(&mut writer, "siri:ServiceRequest");
    write_text_element(&mut writer, "siri:RequestTimestamp", &timestamp);
    write_text_element(&mut writer, "siri:RequestorRef", "sbb-mcp");
    write_start(&mut writer, "OJPTripRequest");
    write_text_element(&mut writer, "RequestTimestamp", &timestamp);

    write_start(&mut writer, "Origin");
    write_start(&mut writer, "PlaceRef");
    write_text_element(&mut writer, "StopPlaceRef", from_station);
    write_text_element(&mut writer, "LocationName", from_station);
    write_end(&mut writer, "PlaceRef");
    write_text_element(&mut writer, "DepArrTime", &departure_time);
    write_end(&mut writer, "Origin");

    write_start(&mut writer, "Destination");
    write_start(&mut writer, "PlaceRef");
    write_text_element(&mut writer, "StopPlaceRef", to_station);
    write_text_element(&mut writer, "LocationName", to_station);
    write_end(&mut writer, "PlaceRef");
    write_end(&mut writer, "Destination");

    write_start(&mut writer, "Params");
    write_text_element(&mut writer, "IncludeRealtimeData", "true");
    write_text_element(&mut writer, "NumberOfResults", "8");
    write_mode_filter(&mut writer, modes);
    write_end(&mut writer, "Params");

    write_end(&mut writer, "OJPTripRequest");
    write_end(&mut writer, "siri:ServiceRequest");
    write_end(&mut writer, "OJPRequest");
    write_end(&mut writer, "OJP");

    String::from_utf8(writer.into_inner())
        .expect("OJP trip request XML should always be valid UTF-8")
}

fn write_mode_filter<W: Write>(writer: &mut Writer<W>, modes: &[TransportMode]) {
    if modes.is_empty() || modes.iter().any(|mode| matches!(mode, TransportMode::All)) {
        return;
    }

    let mut has_modes = false;

    for mode in modes {
        let ojp_mode = match mode {
            TransportMode::Rail => "rail",
            TransportMode::Bus => "bus",
            TransportMode::Tram => "tram",
            TransportMode::Ship => "water",
            TransportMode::Cableway => "cableway",
            TransportMode::Funicular => "funicular",
            TransportMode::All => continue,
        };

        if !has_modes {
            write_start(writer, "TransportModes");
            has_modes = true;
        }

        write_text_element(writer, "PtMode", ojp_mode);
    }

    if has_modes {
        write_end(writer, "TransportModes");
    }
}

fn build_stop_event_request(station_id: &str, limit: usize, window_minutes: i64) -> String {
    let timestamp = Utc::now().with_timezone(&Zurich).to_rfc3339();
    let end = (Utc::now() + chrono::Duration::minutes(window_minutes.max(1)))
        .with_timezone(&Zurich)
        .to_rfc3339();
    let limit_value = limit.to_string();
    let mut writer = Writer::new(Vec::<u8>::new());

    write_ojp_root_start(&mut writer);
    write_start(&mut writer, "OJPRequest");
    write_start(&mut writer, "siri:ServiceRequest");
    write_text_element(&mut writer, "siri:RequestTimestamp", &timestamp);
    write_text_element(&mut writer, "siri:RequestorRef", "sbb-mcp");
    write_start(&mut writer, "OJPStopEventRequest");
    write_text_element(&mut writer, "RequestTimestamp", &timestamp);

    write_start(&mut writer, "Location");
    write_start(&mut writer, "PlaceRef");
    write_text_element(&mut writer, "StopPlaceRef", station_id);
    write_end(&mut writer, "PlaceRef");
    write_text_element(&mut writer, "DepArrTime", &timestamp);
    write_end(&mut writer, "Location");

    write_start(&mut writer, "Params");
    write_text_element(&mut writer, "NumberOfResults", &limit_value);
    write_text_element(&mut writer, "StopEventType", "departure");
    write_text_element(&mut writer, "IncludeRealtimeData", "true");
    write_text_element(&mut writer, "TimeWindow", &end);
    write_end(&mut writer, "Params");

    write_end(&mut writer, "OJPStopEventRequest");
    write_end(&mut writer, "siri:ServiceRequest");
    write_end(&mut writer, "OJPRequest");
    write_end(&mut writer, "OJP");

    String::from_utf8(writer.into_inner())
        .expect("OJP stop event request XML should always be valid UTF-8")
}

fn write_start<W: Write>(writer: &mut Writer<W>, tag: &str) {
    writer
        .write_event(Event::Start(BytesStart::new(tag)))
        .expect("writing XML start tag to in-memory buffer should not fail");
}

fn write_end<W: Write>(writer: &mut Writer<W>, tag: &str) {
    writer
        .write_event(Event::End(BytesEnd::new(tag)))
        .expect("writing XML end tag to in-memory buffer should not fail");
}

fn write_text_element<W: Write>(writer: &mut Writer<W>, tag: &str, value: &str) {
    write_start(writer, tag);
    writer
        .write_event(Event::Text(BytesText::new(value)))
        .expect("writing XML text node to in-memory buffer should not fail");
    write_end(writer, tag);
}

fn write_ojp_root_start<W: Write>(writer: &mut Writer<W>) {
    let mut ojp = BytesStart::new("OJP");
    ojp.push_attribute(("xmlns", "http://www.vdv.de/ojp"));
    ojp.push_attribute(("version", "2.0"));
    ojp.push_attribute(("xmlns:siri", "http://www.siri.org.uk/siri"));
    writer
        .write_event(Event::Start(ojp))
        .expect("writing OJP root element to in-memory buffer should not fail");
}

fn parse_station_response(xml: &str, limit: usize) -> Result<Vec<Station>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack: Vec<String> = Vec::new();

    struct Candidate {
        id: Option<String>,
        name: Option<String>,
        latitude: Option<f64>,
        longitude: Option<f64>,
        station_type: StationType,
    }

    let mut current: Option<Candidate> = None;
    let mut output = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = local_name(start.name());
                stack.push(name.clone());

                if is_station_container(&name) {
                    let station_type = station_type_from_tag(&name);
                    current = Some(Candidate {
                        id: None,
                        name: None,
                        latitude: None,
                        longitude: None,
                        station_type,
                    });
                }
            }
            Event::End(end) => {
                let name = local_name(end.name());
                if is_station_container(&name) {
                    if let Some(candidate) = current.take() {
                        if let Some(name) = candidate.name {
                            output.push(Station {
                                id: candidate.id.unwrap_or_else(|| name.clone()),
                                name,
                                latitude: candidate.latitude,
                                longitude: candidate.longitude,
                                station_type: candidate.station_type,
                            });
                        }
                    }
                }

                let _ = stack.pop();
            }
            Event::Text(text) => {
                let Some(candidate) = current.as_mut() else {
                    continue;
                };
                let value = String::from_utf8_lossy(text.as_ref()).trim().to_string();
                if value.is_empty() {
                    continue;
                }

                let current_tag = stack.last().cloned().unwrap_or_default();
                if matches!(current_tag.as_str(), "StopPlaceRef" | "StopPointRef" | "id") {
                    if candidate.id.is_none() {
                        candidate.id = Some(value);
                    }
                } else if matches!(
                    current_tag.as_str(),
                    "LocationName" | "Name" | "Text" | "TopographicPlaceName"
                ) {
                    if candidate.name.is_none() {
                        candidate.name = Some(value);
                    }
                } else if current_tag == "Longitude" {
                    candidate.longitude = value.parse::<f64>().ok();
                } else if current_tag == "Latitude" {
                    candidate.latitude = value.parse::<f64>().ok();
                }
            }
            Event::Eof => break,
            _ => {}
        }

        buffer.clear();
    }

    let mut deduped = Vec::new();
    let mut seen_ids = HashSet::new();
    for station in output {
        if seen_ids.insert(station.id.clone()) {
            deduped.push(station);
        }
    }

    deduped.truncate(limit);
    Ok(deduped)
}

fn parse_trip_response(xml: &str) -> Result<Vec<Trip>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack: Vec<String> = Vec::new();

    #[derive(Clone)]
    struct TempTrip {
        id: String,
        legs: Vec<TripLeg>,
    }

    let mut current_trip: Option<TempTrip> = None;
    let mut current_leg: Option<TempLeg> = None;
    let mut trips: Vec<Trip> = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let tag = local_name(start.name());
                stack.push(tag.clone());

                if tag == "TripResult" {
                    current_trip = Some(TempTrip {
                        id: uuid::Uuid::new_v4().to_string(),
                        legs: Vec::new(),
                    });
                } else if matches!(tag.as_str(), "TripLeg" | "Leg") {
                    current_leg = Some(TempLeg::default());
                }
            }
            Event::End(end) => {
                let tag = local_name(end.name());
                if matches!(tag.as_str(), "TripLeg" | "Leg") {
                    if let (Some(trip), Some(leg)) = (current_trip.as_mut(), current_leg.take()) {
                        if let Some(mapped) = map_leg(leg) {
                            trip.legs.push(mapped);
                        }
                    }
                } else if tag == "TripResult" {
                    if let Some(trip) = current_trip.take() {
                        if !trip.legs.is_empty() {
                            let departure = trip
                                .legs
                                .first()
                                .map(|leg| leg.departure.scheduled)
                                .unwrap_or_else(now_fixed);
                            let arrival = trip
                                .legs
                                .last()
                                .map(|leg| leg.arrival.expected)
                                .unwrap_or_else(now_fixed);
                            let duration_minutes = (arrival - departure).num_minutes().max(0);

                            trips.push(Trip {
                                id: trip.id,
                                duration_minutes,
                                legs: trip.legs,
                            });
                        }
                    }
                }

                let _ = stack.pop();
            }
            Event::Text(text) => {
                let Some(leg) = current_leg.as_mut() else {
                    continue;
                };
                let value = String::from_utf8_lossy(text.as_ref()).trim().to_string();
                if value.is_empty() {
                    continue;
                }

                let current = stack.last().cloned().unwrap_or_default();
                let in_origin = path_contains(&stack, &["Origin", "From", "Board"]);
                let in_destination = path_contains(&stack, &["Destination", "To", "Alight"]);
                let in_departure = path_contains(&stack, &["Departure", "ServiceDeparture"]);
                let in_arrival = path_contains(&stack, &["Arrival", "ServiceArrival"]);

                match current.as_str() {
                    "Mode" | "PtMode" => {
                        if leg.mode.is_none() {
                            leg.mode = Some(value);
                        }
                    }
                    "PublishedLineName" | "LineRef" => {
                        if leg.line.is_none() {
                            leg.line = Some(value);
                        }
                    }
                    "OperatorRef" | "OperatorName" => {
                        if leg.operator.is_none() {
                            leg.operator = Some(value);
                        }
                    }
                    "JourneyRef" => {
                        if let Some(trip) = current_trip.as_mut() {
                            trip.id = value;
                        }
                    }
                    "LocationName" | "StopPointName" | "Name" | "Text" => {
                        if in_origin && leg.from_station.is_none() {
                            leg.from_station = Some(value);
                        } else if in_destination && leg.to_station.is_none() {
                            leg.to_station = Some(value);
                        }
                    }
                    "PlannedQuay" | "EstimatedQuay" | "Platform" => {
                        if in_origin && leg.from_platform.is_none() {
                            leg.from_platform = Some(value);
                        } else if in_destination && leg.to_platform.is_none() {
                            leg.to_platform = Some(value);
                        }
                    }
                    "TimetabledTime" => {
                        if let Some(time) = parse_ojp_datetime(&value) {
                            if in_departure {
                                leg.departure_scheduled = Some(time);
                            } else if in_arrival {
                                leg.arrival_scheduled = Some(time);
                            }
                        }
                    }
                    "EstimatedTime" => {
                        if let Some(time) = parse_ojp_datetime(&value) {
                            if in_departure {
                                leg.departure_expected = Some(time);
                            } else if in_arrival {
                                leg.arrival_expected = Some(time);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }

        buffer.clear();
    }

    Ok(trips)
}

fn parse_departure_response(xml: &str, limit: usize) -> Result<Vec<Departure>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack: Vec<String> = Vec::new();

    #[derive(Default)]
    struct TempDeparture {
        trip_id: Option<String>,
        line: Option<String>,
        destination: Option<String>,
        mode: Option<String>,
        scheduled: Option<DateTime<FixedOffset>>,
        expected: Option<DateTime<FixedOffset>>,
        platform: Option<String>,
        cancelled: bool,
    }

    let mut current: Option<TempDeparture> = None;
    let mut departures = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let tag = local_name(start.name());
                stack.push(tag.clone());

                if matches!(tag.as_str(), "StopEventResult" | "StopEvent") {
                    current = Some(TempDeparture::default());
                }
            }
            Event::End(end) => {
                let tag = local_name(end.name());
                if matches!(tag.as_str(), "StopEventResult" | "StopEvent") {
                    if let Some(dep) = current.take() {
                        let scheduled = dep.scheduled.unwrap_or_else(now_fixed);
                        let expected = dep.expected.unwrap_or(scheduled);
                        let delay_minutes = ((expected - scheduled).num_minutes()) as i32;
                        let status = if dep.cancelled {
                            TripStatus::Cancelled
                        } else {
                            TripStatus::from_delay(delay_minutes)
                        };

                        departures.push(Departure {
                            trip_id: dep.trip_id,
                            line: dep.line,
                            destination: dep
                                .destination
                                .unwrap_or_else(|| "Unknown destination".to_string()),
                            mode: dep.mode,
                            scheduled,
                            expected,
                            delay_minutes,
                            platform: dep.platform,
                            status,
                        });
                    }
                }

                let _ = stack.pop();
            }
            Event::Text(text) => {
                let Some(dep) = current.as_mut() else {
                    continue;
                };

                let value = String::from_utf8_lossy(text.as_ref()).trim().to_string();
                if value.is_empty() {
                    continue;
                }

                let current_tag = stack.last().cloned().unwrap_or_default();
                let in_destination = path_contains(&stack, &["Destination", "To"]);

                match current_tag.as_str() {
                    "JourneyRef" => dep.trip_id = Some(value),
                    "PublishedLineName" | "LineRef" => dep.line = Some(value),
                    "Mode" | "PtMode" => dep.mode = Some(value),
                    "LocationName" | "StopPointName" | "Name" | "Text" => {
                        if in_destination && dep.destination.is_none() {
                            dep.destination = Some(value);
                        }
                    }
                    "TimetabledTime" => {
                        if dep.scheduled.is_none() {
                            dep.scheduled = parse_ojp_datetime(&value);
                        }
                    }
                    "EstimatedTime" => dep.expected = parse_ojp_datetime(&value),
                    "PlannedQuay" | "EstimatedQuay" | "Platform" => dep.platform = Some(value),
                    "DatedJourneyStatus" | "Status" => {
                        if value.to_ascii_lowercase().contains("cancel") {
                            dep.cancelled = true;
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }

        buffer.clear();
    }

    departures.sort_by_key(|dep| dep.expected);
    departures.truncate(limit);
    Ok(departures)
}

fn map_leg(leg: TempLeg) -> Option<TripLeg> {
    let departure_scheduled = leg.departure_scheduled.or_else(|| leg.departure_expected)?;
    let arrival_scheduled = leg.arrival_scheduled.or_else(|| leg.arrival_expected)?;
    let departure_expected = leg.departure_expected.unwrap_or(departure_scheduled);
    let arrival_expected = leg.arrival_expected.unwrap_or(arrival_scheduled);

    let departure_delay = (departure_expected - departure_scheduled).num_minutes() as i32;
    let arrival_delay = (arrival_expected - arrival_scheduled).num_minutes() as i32;

    let status = if leg.from_platform != leg.to_platform
        && leg.from_platform.is_some()
        && leg.to_platform.is_some()
    {
        TripStatus::PlatformChanged
    } else {
        TripStatus::from_delay(departure_delay.max(arrival_delay))
    };

    Some(TripLeg {
        mode: leg.mode.unwrap_or_else(|| "rail".to_string()),
        line: leg.line,
        operator: leg.operator,
        from: LegStop {
            station: leg.from_station.unwrap_or_else(|| "Unknown".to_string()),
            platform: leg.from_platform,
        },
        to: LegStop {
            station: leg.to_station.unwrap_or_else(|| "Unknown".to_string()),
            platform: leg.to_platform,
        },
        departure: TimeInfo {
            scheduled: departure_scheduled,
            expected: departure_expected,
            delay_minutes: departure_delay,
        },
        arrival: TimeInfo {
            scheduled: arrival_scheduled,
            expected: arrival_expected,
            delay_minutes: arrival_delay,
        },
        status,
    })
}

fn parse_ojp_datetime(value: &str) -> Option<DateTime<FixedOffset>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Some(dt);
    }

    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S") {
        return Zurich
            .from_local_datetime(&naive)
            .earliest()
            .map(|datetime| datetime.fixed_offset());
    }

    None
}

fn is_station_container(tag: &str) -> bool {
    matches!(
        tag,
        "StopPlace" | "StopPoint" | "Address" | "PointOfInterest"
    )
}

fn station_type_from_tag(tag: &str) -> StationType {
    match tag {
        "StopPlace" | "StopPoint" => StationType::Stop,
        "Address" => StationType::Address,
        "PointOfInterest" => StationType::Poi,
        _ => StationType::Unknown,
    }
}

fn local_name(name: QName<'_>) -> String {
    let raw = String::from_utf8_lossy(name.as_ref());
    raw.split(':').next_back().unwrap_or_default().to_string()
}

fn path_contains(stack: &[String], candidates: &[&str]) -> bool {
    stack
        .iter()
        .rev()
        .take(4)
        .any(|segment| candidates.iter().any(|candidate| segment == candidate))
}

fn now_fixed() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&Zurich).fixed_offset()
}
