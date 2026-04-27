use std::collections::{HashMap, HashSet};
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
    Departure, GeoPoint, LegStop, Station, StationType, StopCallTime, TimeInfo, ToolWarning,
    TransportMode, Trip, TripLeg, TripLegOccupancy, TripLegStopCall, TripStatus,
    format_service_label,
};

#[derive(Default, Clone)]
struct TempLeg {
    mode: Option<String>,
    line: Option<String>,
    operator: Option<String>,
    stop_sequence: Vec<String>,
    from_station: Option<String>,
    from_stop_ref: Option<String>,
    from_platform: Option<String>,
    from_latitude: Option<f64>,
    from_longitude: Option<f64>,
    to_station: Option<String>,
    to_stop_ref: Option<String>,
    to_platform: Option<String>,
    to_latitude: Option<f64>,
    to_longitude: Option<f64>,
    departure_scheduled: Option<DateTime<FixedOffset>>,
    departure_expected: Option<DateTime<FixedOffset>>,
    arrival_scheduled: Option<DateTime<FixedOffset>>,
    arrival_expected: Option<DateTime<FixedOffset>>,
    fallback_arrival_scheduled: Option<DateTime<FixedOffset>>,
    fallback_arrival_expected: Option<DateTime<FixedOffset>>,
    occupancy_first_class: Option<String>,
    occupancy_second_class: Option<String>,
    occupancy_general: Option<String>,
    stop_calls: Vec<TempStopCall>,
}

#[derive(Default, Clone)]
struct TempStopCall {
    station: Option<String>,
    stop_point_ref: Option<String>,
    platform_scheduled: Option<String>,
    platform_expected: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    arrival_scheduled: Option<DateTime<FixedOffset>>,
    arrival_expected: Option<DateTime<FixedOffset>>,
    departure_scheduled: Option<DateTime<FixedOffset>>,
    departure_expected: Option<DateTime<FixedOffset>>,
}

#[derive(Default, Clone)]
struct TempOccupancy {
    fare_class: Option<String>,
    level: Option<String>,
}

#[derive(Default, Clone)]
struct TempPlace {
    stop_place_ref: Option<String>,
    stop_point_ref: Option<String>,
    name: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum PlatformKind {
    Scheduled,
    Expected,
    Generic,
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

    pub async fn plan_trip_with_refs(
        &self,
        from_station_name: &str,
        from_station_ref: &str,
        to_station_name: &str,
        to_station_ref: &str,
        datetime: DateTime<FixedOffset>,
        modes: &[TransportMode],
        max_results: usize,
    ) -> Result<(Vec<Trip>, Option<ToolWarning>)> {
        let mode_key = modes
            .iter()
            .map(TransportMode::as_str)
            .collect::<Vec<_>>()
            .join(",");
        let cache_key = format!(
            "{}:{}:{}:{}:{}",
            from_station_ref.to_ascii_lowercase(),
            to_station_ref.to_ascii_lowercase(),
            datetime.to_rfc3339(),
            mode_key,
            max_results
        );

        {
            let mut cache = self.trip_cache.write().await;
            if let Some(trips) = cache.get(&cache_key) {
                return Ok((trips, None));
            }
        }

        let xml = build_trip_request(
            from_station_name,
            from_station_ref,
            to_station_name,
            to_station_ref,
            datetime,
            modes,
            max_results,
        );
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
    write_text_element(&mut writer, "siri:RequestorRef", "sbb_mcp_prod");
    write_start(&mut writer, "OJPLocationInformationRequest");
    write_text_element(&mut writer, "siri:RequestTimestamp", &timestamp);
    write_start(&mut writer, "InitialInput");
    write_text_element(&mut writer, "Name", query);
    write_end(&mut writer, "InitialInput");
    write_start(&mut writer, "Restrictions");
    write_text_element(&mut writer, "Type", "stop");
    write_text_element(&mut writer, "NumberOfResults", &limit_value);
    write_text_element(&mut writer, "IncludePtModes", "true");
    write_end(&mut writer, "Restrictions");
    write_end(&mut writer, "OJPLocationInformationRequest");
    write_end(&mut writer, "siri:ServiceRequest");
    write_end(&mut writer, "OJPRequest");
    write_end(&mut writer, "OJP");

    String::from_utf8(writer.into_inner())
        .expect("OJP location request XML should always be valid UTF-8")
}

fn build_trip_request(
    from_station_name: &str,
    from_station_ref: &str,
    to_station_name: &str,
    to_station_ref: &str,
    datetime: DateTime<FixedOffset>,
    modes: &[TransportMode],
    max_results: usize,
) -> String {
    let timestamp = Utc::now().with_timezone(&Zurich).to_rfc3339();
    let departure_time = datetime.to_rfc3339();
    let mut writer = Writer::new(Vec::<u8>::new());

    write_ojp_root_start(&mut writer);
    write_start(&mut writer, "OJPRequest");
    write_start(&mut writer, "siri:ServiceRequest");
    write_text_element(&mut writer, "siri:RequestTimestamp", &timestamp);
    write_text_element(&mut writer, "siri:RequestorRef", "sbb_mcp_prod");
    write_start(&mut writer, "OJPTripRequest");
    write_text_element(&mut writer, "siri:RequestTimestamp", &timestamp);

    write_start(&mut writer, "Origin");
    write_place_ref(&mut writer, from_station_name, from_station_ref);
    write_text_element(&mut writer, "DepArrTime", &departure_time);
    write_end(&mut writer, "Origin");

    write_start(&mut writer, "Destination");
    write_place_ref(&mut writer, to_station_name, to_station_ref);
    write_end(&mut writer, "Destination");

    write_start(&mut writer, "Params");
    write_text_element(
        &mut writer,
        "NumberOfResults",
        &max_results.clamp(1, 5).to_string(),
    );
    write_text_element(&mut writer, "UseRealtimeData", "full");
    write_text_element(&mut writer, "IncludeIntermediateStops", "true");
    write_text_element(&mut writer, "IncludePreviousCalls", "true");
    write_text_element(&mut writer, "IncludeOnwardCalls", "true");
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

    write_start(writer, "ModeAndModeOfOperationFilter");
    write_text_element(writer, "Exclude", "false");

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

        write_text_element(writer, "PtMode", ojp_mode);
    }

    write_end(writer, "ModeAndModeOfOperationFilter");
}

fn build_stop_event_request(station_id: &str, limit: usize, window_minutes: i64) -> String {
    let timestamp = Utc::now().with_timezone(&Zurich).to_rfc3339();
    let _window_minutes = window_minutes.max(1);
    let limit_value = limit.to_string();
    let mut writer = Writer::new(Vec::<u8>::new());

    write_ojp_root_start(&mut writer);
    write_start(&mut writer, "OJPRequest");
    write_start(&mut writer, "siri:ServiceRequest");
    write_text_element(&mut writer, "siri:RequestTimestamp", &timestamp);
    write_text_element(&mut writer, "siri:RequestorRef", "sbb_mcp_prod");
    write_start(&mut writer, "OJPStopEventRequest");
    write_text_element(&mut writer, "siri:RequestTimestamp", &timestamp);

    write_start(&mut writer, "Location");
    write_place_ref(&mut writer, station_id, station_id);
    write_text_element(&mut writer, "DepArrTime", &timestamp);
    write_end(&mut writer, "Location");

    write_start(&mut writer, "Params");
    write_text_element(&mut writer, "NumberOfResults", &limit_value);
    write_text_element(&mut writer, "StopEventType", "departure");
    write_text_element(&mut writer, "IncludePreviousCalls", "false");
    write_text_element(&mut writer, "IncludeOnwardCalls", "false");
    write_text_element(&mut writer, "UseRealtimeData", "full");
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

fn write_place_ref<W: Write>(writer: &mut Writer<W>, name: &str, stop_ref: &str) {
    write_start(writer, "PlaceRef");
    if looks_like_stop_ref(stop_ref) {
        write_text_element(writer, "StopPlaceRef", stop_ref);
    }

    write_start(writer, "Name");
    write_text_element(writer, "Text", name);
    write_end(writer, "Name");
    write_end(writer, "PlaceRef");
}

fn looks_like_stop_ref(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && (trimmed.chars().all(|ch| ch.is_ascii_digit())
            || trimmed.starts_with("ch:")
            || trimmed.contains(":sloid:"))
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
        finalize_on_location_end: bool,
    }

    let mut current: Option<Candidate> = None;
    let mut output = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = local_name(start.name());
                stack.push(name.clone());

                // OJP LocationInformation response wraps each result as Location/Location.
                // The inner Location carries place metadata and a sibling GeoPosition.
                if is_location_result_container(&stack, &name) {
                    current = Some(Candidate {
                        id: None,
                        name: None,
                        latitude: None,
                        longitude: None,
                        station_type: StationType::Unknown,
                        finalize_on_location_end: true,
                    });
                }

                if is_station_container(&name) {
                    if let Some(candidate) = current.as_mut() {
                        candidate.station_type = station_type_from_tag(&name);
                    } else {
                        current = Some(Candidate {
                            id: None,
                            name: None,
                            latitude: None,
                            longitude: None,
                            station_type: station_type_from_tag(&name),
                            finalize_on_location_end: false,
                        });
                    }
                }
            }
            Event::End(end) => {
                let name = local_name(end.name());
                if is_location_result_container(&stack, &name)
                    && current
                        .as_ref()
                        .is_some_and(|candidate| candidate.finalize_on_location_end)
                {
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
                } else if is_station_container(&name)
                    && current
                        .as_ref()
                        .is_some_and(|candidate| !candidate.finalize_on_location_end)
                {
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
                if matches!(
                    current_tag.as_str(),
                    "StopPlaceRef" | "StopPointRef" | "TopographicPlaceCode" | "AddressCode" | "id"
                ) {
                    if candidate.id.is_none() {
                        candidate.id = Some(value);
                    }
                } else if matches!(
                    current_tag.as_str(),
                    "StopPlaceName"
                        | "StopPointName"
                        | "AddressName"
                        | "PointOfInterestName"
                        | "LocationName"
                        | "TopographicPlaceName"
                        | "Name"
                        | "Text"
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
    let mut current_stop_call: Option<TempStopCall> = None;
    let mut current_occupancy: Option<TempOccupancy> = None;
    let mut current_place: Option<TempPlace> = None;
    let mut trips: Vec<Trip> = Vec::new();
    let mut coordinates_by_ref: HashMap<String, GeoPoint> = HashMap::new();
    let mut coordinates_by_name: HashMap<String, GeoPoint> = HashMap::new();

    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let tag = local_name(start.name());
                stack.push(tag.clone());
                let in_trip_context_places = path_contains(&stack, &["TripResponseContext"])
                    && path_contains(&stack, &["Places"]);

                if tag == "Place" && in_trip_context_places {
                    current_place = Some(TempPlace::default());
                } else if tag == "TripResult" {
                    current_trip = Some(TempTrip {
                        id: uuid::Uuid::new_v4().to_string(),
                        legs: Vec::new(),
                    });
                } else if matches!(tag.as_str(), "TripLeg" | "Leg") {
                    current_leg = Some(TempLeg::default());
                } else if is_stop_call_container(&tag) {
                    current_stop_call = Some(TempStopCall::default());
                } else if tag == "ExpectedDepartureOccupancy" {
                    current_occupancy = Some(TempOccupancy::default());
                }
            }
            Event::End(end) => {
                let tag = local_name(end.name());
                if tag == "Place" {
                    if let Some(place) = current_place.take()
                        && let Some(point) = build_geo_point(place.latitude, place.longitude)
                    {
                        if let Some(stop_ref) = place.stop_point_ref {
                            coordinates_by_ref
                                .insert(normalize_lookup_key(&stop_ref), point.clone());
                        }
                        if let Some(stop_place_ref) = place.stop_place_ref {
                            coordinates_by_ref
                                .insert(normalize_lookup_key(&stop_place_ref), point.clone());
                        }
                        if let Some(name) = place.name {
                            coordinates_by_name.insert(normalize_lookup_key(&name), point);
                        }
                    }
                } else if matches!(tag.as_str(), "TripLeg" | "Leg") {
                    if let (Some(trip), Some(leg)) = (current_trip.as_mut(), current_leg.take()) {
                        let mut leg = leg;
                        enrich_temp_leg_coordinates(
                            &mut leg,
                            &coordinates_by_ref,
                            &coordinates_by_name,
                        );
                        if let Some(mapped) = map_leg(leg) {
                            trip.legs.push(mapped);
                        }
                    }
                } else if is_stop_call_container(&tag) {
                    if let (Some(leg), Some(stop_call)) =
                        (current_leg.as_mut(), current_stop_call.take())
                    {
                        leg.stop_calls.push(stop_call);
                    }
                } else if tag == "ExpectedDepartureOccupancy" {
                    if let (Some(leg), Some(occupancy)) =
                        (current_leg.as_mut(), current_occupancy.take())
                    {
                        let level = occupancy
                            .level
                            .as_deref()
                            .and_then(normalize_occupancy_level);
                        if let Some(level) = level {
                            let fare = occupancy
                                .fare_class
                                .unwrap_or_default()
                                .to_ascii_lowercase();
                            if fare.contains("first") {
                                if leg.occupancy_first_class.is_none() {
                                    leg.occupancy_first_class = Some(level);
                                }
                            } else if fare.contains("second") {
                                if leg.occupancy_second_class.is_none() {
                                    leg.occupancy_second_class = Some(level);
                                }
                            } else if leg.occupancy_general.is_none() {
                                leg.occupancy_general = Some(level);
                            }
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
                let value = String::from_utf8_lossy(text.as_ref()).trim().to_string();
                if value.is_empty() {
                    continue;
                }

                let current = stack.last().cloned().unwrap_or_default();
                if let Some(place) = current_place.as_mut() {
                    match current.as_str() {
                        "StopPlaceRef" => {
                            if place.stop_place_ref.is_none() {
                                place.stop_place_ref = Some(value.clone());
                            }
                        }
                        "StopPointRef" => {
                            if place.stop_point_ref.is_none() {
                                place.stop_point_ref = Some(value.clone());
                            }
                        }
                        "StopPlaceName"
                        | "StopPointName"
                        | "TopographicPlaceName"
                        | "LocationName"
                        | "Name"
                        | "Text" => {
                            if place.name.is_none() {
                                place.name = Some(value.clone());
                            }
                        }
                        "Longitude" => place.longitude = value.parse::<f64>().ok(),
                        "Latitude" => place.latitude = value.parse::<f64>().ok(),
                        _ => {}
                    }
                }

                let Some(leg) = current_leg.as_mut() else {
                    continue;
                };
                let in_origin = path_contains(&stack, &["Origin", "From", "Board"]);
                let in_destination = path_contains(&stack, &["Destination", "To", "Alight"]);
                let in_departure = path_contains(&stack, &["Departure", "ServiceDeparture"]);
                let in_arrival = path_contains(&stack, &["Arrival", "ServiceArrival"]);
                let in_board = path_contains(&stack, &["LegBoard", "Origin", "From"]);
                let in_alight = path_contains(&stack, &["LegAlight", "Destination", "To"]);
                let in_leg_stop = path_contains(
                    &stack,
                    &[
                        "LegBoard",
                        "LegIntermediate",
                        "LegAlight",
                        "CallAtStop",
                        "IntermediateStop",
                        "OnwardCall",
                        "PreviousCall",
                        "LegStart",
                        "LegEnd",
                    ],
                );
                let in_call_arrival = path_contains(&stack, &["ServiceArrival"]);
                let in_call_departure = path_contains(&stack, &["ServiceDeparture"]);
                let in_call_stop_name = path_contains(&stack, &["StopPointName", "LocationName"]);

                if let Some(stop_call) = current_stop_call.as_mut() {
                    if in_call_stop_name
                        && matches!(
                            current.as_str(),
                            "StopPointName" | "LocationName" | "Name" | "Text"
                        )
                    {
                        stop_call.station = Some(value.clone());
                    }

                    if current == "TimetabledTime" {
                        if let Some(time) = parse_ojp_datetime(&value) {
                            if in_call_arrival {
                                stop_call.arrival_scheduled = Some(time);
                            } else if in_call_departure {
                                stop_call.departure_scheduled = Some(time);
                            }
                        }
                    }

                    if current == "EstimatedTime" {
                        if let Some(time) = parse_ojp_datetime(&value) {
                            if in_call_arrival {
                                stop_call.arrival_expected = Some(time);
                            } else if in_call_departure {
                                stop_call.departure_expected = Some(time);
                            }
                        }
                    }

                    if current == "StopPointRef" && stop_call.stop_point_ref.is_none() {
                        stop_call.stop_point_ref = Some(value.clone());
                    }

                    if current == "Longitude"
                        && (in_call_arrival || in_call_departure || in_leg_stop)
                    {
                        stop_call.longitude = value.parse::<f64>().ok();
                    }
                    if current == "Latitude"
                        && (in_call_arrival || in_call_departure || in_leg_stop)
                    {
                        stop_call.latitude = value.parse::<f64>().ok();
                    }

                    if (in_call_arrival || in_call_departure || in_leg_stop)
                        && let Some(kind) = detect_platform_kind(&stack, &current)
                    {
                        set_platform_candidate(
                            &mut stop_call.platform_scheduled,
                            &mut stop_call.platform_expected,
                            kind,
                            value.clone(),
                        );
                    }
                }

                if let Some(occupancy) = current_occupancy.as_mut() {
                    match current.as_str() {
                        "FareClass" => occupancy.fare_class = Some(value.clone()),
                        "OccupancyLevel" => occupancy.level = Some(value.clone()),
                        _ => {}
                    }
                }

                match current.as_str() {
                    "Mode" | "PtMode" => {
                        if leg.mode.is_none() {
                            leg.mode = Some(value);
                        }
                    }
                    "PublicCode" | "PublishedLineName" => {
                        set_preferred_line(&mut leg.line, value);
                    }
                    "LineRef" => {
                        // Keep technical line refs out of rider-facing output.
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
                        if current == "Text" && path_contains(&stack, &["PublishedServiceName"]) {
                            set_preferred_line(&mut leg.line, value);
                            continue;
                        }

                        if in_leg_stop
                            && (path_contains(&stack, &["StopPointName", "LocationName", "Name"])
                                || current != "Text")
                        {
                            leg.stop_sequence.push(value.clone());
                        }

                        if in_origin && leg.from_station.is_none() {
                            leg.from_station = Some(value);
                        } else if in_destination && leg.to_station.is_none() {
                            leg.to_station = Some(value);
                        }
                    }
                    "StopPointRef" => {
                        if in_origin && leg.from_platform.is_none() {
                            leg.from_platform = platform_from_stop_point_ref(&value);
                            if leg.from_stop_ref.is_none() {
                                leg.from_stop_ref = Some(value.clone());
                            }
                        } else if in_destination && leg.to_platform.is_none() {
                            leg.to_platform = platform_from_stop_point_ref(&value);
                            if leg.to_stop_ref.is_none() {
                                leg.to_stop_ref = Some(value.clone());
                            }
                        }
                    }
                    _ if detect_platform_kind(&stack, &current).is_some() => {
                        if in_origin {
                            if let Some(platform) = normalize_platform_value(&value) {
                                leg.from_platform.get_or_insert(platform);
                            }
                        } else if in_destination
                            && let Some(platform) = normalize_platform_value(&value)
                        {
                            leg.to_platform.get_or_insert(platform);
                        }
                    }
                    "Longitude" => {
                        if let Ok(parsed) = value.parse::<f64>() {
                            if in_origin {
                                leg.from_longitude = Some(parsed);
                            } else if in_destination {
                                leg.to_longitude = Some(parsed);
                            }
                        }
                    }
                    "Latitude" => {
                        if let Ok(parsed) = value.parse::<f64>() {
                            if in_origin {
                                leg.from_latitude = Some(parsed);
                            } else if in_destination {
                                leg.to_latitude = Some(parsed);
                            }
                        }
                    }
                    "TimetabledTime" => {
                        if let Some(time) = parse_ojp_datetime(&value) {
                            if in_board || (in_departure && leg.departure_scheduled.is_none()) {
                                leg.departure_scheduled = Some(time);
                            } else if in_alight {
                                leg.arrival_scheduled = Some(time);
                            } else if in_arrival {
                                leg.fallback_arrival_scheduled = Some(time);
                            }
                        }
                    }
                    "EstimatedTime" => {
                        if let Some(time) = parse_ojp_datetime(&value) {
                            if in_board || (in_departure && leg.departure_expected.is_none()) {
                                leg.departure_expected = Some(time);
                            } else if in_alight {
                                leg.arrival_expected = Some(time);
                            } else if in_arrival {
                                leg.fallback_arrival_expected = Some(time);
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

fn normalize_lookup_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn enrich_temp_leg_coordinates(
    leg: &mut TempLeg,
    coordinates_by_ref: &HashMap<String, GeoPoint>,
    coordinates_by_name: &HashMap<String, GeoPoint>,
) {
    if leg.from_latitude.is_none() || leg.from_longitude.is_none() {
        let point = leg
            .from_stop_ref
            .as_deref()
            .map(normalize_lookup_key)
            .and_then(|key| coordinates_by_ref.get(&key).cloned())
            .or_else(|| {
                leg.from_station
                    .as_deref()
                    .map(normalize_lookup_key)
                    .and_then(|key| coordinates_by_name.get(&key).cloned())
            });
        if let Some(point) = point {
            leg.from_latitude.get_or_insert(point.latitude);
            leg.from_longitude.get_or_insert(point.longitude);
        }
    }

    if leg.to_latitude.is_none() || leg.to_longitude.is_none() {
        let point = leg
            .to_stop_ref
            .as_deref()
            .map(normalize_lookup_key)
            .and_then(|key| coordinates_by_ref.get(&key).cloned())
            .or_else(|| {
                leg.to_station
                    .as_deref()
                    .map(normalize_lookup_key)
                    .and_then(|key| coordinates_by_name.get(&key).cloned())
            });
        if let Some(point) = point {
            leg.to_latitude.get_or_insert(point.latitude);
            leg.to_longitude.get_or_insert(point.longitude);
        }
    }

    for call in &mut leg.stop_calls {
        if call.latitude.is_some() && call.longitude.is_some() {
            continue;
        }

        let point = call
            .stop_point_ref
            .as_deref()
            .map(normalize_lookup_key)
            .and_then(|key| coordinates_by_ref.get(&key).cloned())
            .or_else(|| {
                call.station
                    .as_deref()
                    .map(normalize_lookup_key)
                    .and_then(|key| coordinates_by_name.get(&key).cloned())
            });

        if let Some(point) = point {
            call.latitude.get_or_insert(point.latitude);
            call.longitude.get_or_insert(point.longitude);
        }
    }
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
                        let line = dep.line;
                        let mode = dep.mode;
                        let service = format_service_label(mode.as_deref(), line.as_deref());
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
                            line,
                            service,
                            destination: dep
                                .destination
                                .unwrap_or_else(|| "Unknown destination".to_string()),
                            mode,
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
                    "PublicCode" | "PublishedLineName" => set_preferred_line(&mut dep.line, value),
                    "LineRef" => {}
                    "Mode" | "PtMode" => dep.mode = Some(value),
                    "LocationName" | "StopPointName" | "Name" | "Text" => {
                        if current_tag == "Text" && path_contains(&stack, &["PublishedServiceName"])
                        {
                            set_preferred_line(&mut dep.line, value);
                            continue;
                        }
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
    let arrival_scheduled = leg
        .arrival_scheduled
        .or(leg.fallback_arrival_scheduled)
        .or(leg.arrival_expected)
        .or(leg.fallback_arrival_expected)?;
    let departure_expected = leg.departure_expected.unwrap_or(departure_scheduled);
    let arrival_expected = leg
        .arrival_expected
        .or(leg.fallback_arrival_expected)
        .unwrap_or(arrival_scheduled);

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

    let mode = leg.mode.unwrap_or_else(|| "rail".to_string());
    let line = leg.line;
    let service = format_service_label(Some(mode.as_str()), line.as_deref());
    let occupancy_general = leg.occupancy_general.clone().or_else(|| {
        aggregate_occupancy_level(&[
            leg.occupancy_first_class.as_deref(),
            leg.occupancy_second_class.as_deref(),
        ])
    });
    let occupancy = if leg.occupancy_first_class.is_some()
        || leg.occupancy_second_class.is_some()
        || occupancy_general.is_some()
    {
        Some(TripLegOccupancy {
            first_class: leg.occupancy_first_class,
            second_class: leg.occupancy_second_class,
            general: occupancy_general,
        })
    } else {
        None
    };

    let mut stop_calls = Vec::new();
    for call in &leg.stop_calls {
        let Some(station_raw) = call.station.as_ref() else {
            continue;
        };
        let station = station_raw.trim();
        if station.is_empty() {
            continue;
        }

        let mut scheduled_platform = call.platform_scheduled.clone();
        let mut expected_platform = call.platform_expected.clone();
        let fallback_platform = call
            .stop_point_ref
            .as_deref()
            .and_then(platform_from_stop_point_ref);
        if expected_platform.is_none() {
            expected_platform = fallback_platform.clone();
        }
        if scheduled_platform.is_none() {
            scheduled_platform = fallback_platform.clone();
        }
        let platform = expected_platform
            .clone()
            .or_else(|| scheduled_platform.clone())
            .or(fallback_platform);
        let coordinates = build_geo_point(call.latitude, call.longitude);

        stop_calls.push(TripLegStopCall {
            station: station.to_string(),
            platform,
            scheduled_platform,
            expected_platform,
            coordinates,
            arrival: build_stop_call_time(call.arrival_scheduled, call.arrival_expected),
            departure: build_stop_call_time(call.departure_scheduled, call.departure_expected),
        });
    }

    let mut deduped_calls: Vec<TripLegStopCall> = Vec::new();
    for call in stop_calls {
        if let Some(last) = deduped_calls.last_mut() {
            if last.station == call.station {
                if last.arrival.is_none() {
                    last.arrival = call.arrival;
                }
                if last.departure.is_none() {
                    last.departure = call.departure;
                }
                if last.platform.is_none() {
                    last.platform = call.platform.clone();
                }
                if last.scheduled_platform.is_none() {
                    last.scheduled_platform = call.scheduled_platform.clone();
                }
                if last.expected_platform.is_none() {
                    last.expected_platform = call.expected_platform.clone();
                }
                continue;
            }
        }
        deduped_calls.push(call);
    }

    let mut stops = Vec::new();
    if !deduped_calls.is_empty() {
        stops.extend(deduped_calls.iter().map(|call| call.station.clone()));
    } else {
        for stop in leg.stop_sequence {
            let trimmed = stop.trim();
            if trimmed.is_empty() {
                continue;
            }
            if stops.last().is_some_and(|prev: &String| prev == trimmed) {
                continue;
            }
            stops.push(trimmed.to_string());
        }
    }

    if let Some(from_station) = leg.from_station.as_ref() {
        if stops.first() != Some(from_station) {
            stops.insert(0, from_station.clone());
        }
    }
    if let Some(to_station) = leg.to_station.as_ref() {
        if stops.last() != Some(to_station) {
            stops.push(to_station.clone());
        }
    }

    Some(TripLeg {
        mode,
        line,
        service,
        operator: leg.operator,
        occupancy,
        stops,
        stop_calls: deduped_calls,
        from: LegStop {
            station: leg.from_station.unwrap_or_else(|| "Unknown".to_string()),
            platform: leg.from_platform,
            coordinates: build_geo_point(leg.from_latitude, leg.from_longitude),
        },
        to: LegStop {
            station: leg.to_station.unwrap_or_else(|| "Unknown".to_string()),
            platform: leg.to_platform,
            coordinates: build_geo_point(leg.to_latitude, leg.to_longitude),
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

fn normalize_occupancy_level(value: &str) -> Option<String> {
    let compact = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();

    match compact.as_str() {
        "manyseatsavailable" | "seatsavailable" | "emptymanyseatsavailable" => {
            Some("low to average".to_string())
        }
        "fewseatsavailable" | "standingroomavailable" | "passengersstanding" => {
            Some("high occupancy".to_string())
        }
        "standingroomonly" | "crushedstandingroomonly" | "full" => {
            Some("very high occupancy".to_string())
        }
        "unknown" | "nodataavailable" | "notacceptingpassengers" | "notboardable" | "empty" => None,
        _ => None,
    }
}

fn aggregate_occupancy_level(levels: &[Option<&str>]) -> Option<String> {
    levels
        .iter()
        .filter_map(|value| *value)
        .max_by_key(|value| occupancy_level_rank(value))
        .map(ToString::to_string)
}

fn occupancy_level_rank(value: &str) -> i32 {
    match value.trim().to_ascii_lowercase().as_str() {
        "very high occupancy" => 3,
        "high occupancy" => 2,
        "low to average" => 1,
        _ => 0,
    }
}

fn build_stop_call_time(
    scheduled: Option<DateTime<FixedOffset>>,
    expected: Option<DateTime<FixedOffset>>,
) -> Option<StopCallTime> {
    let scheduled = scheduled.or(expected)?;
    let expected = expected.unwrap_or(scheduled);
    Some(StopCallTime {
        scheduled,
        expected,
        delay_minutes: (expected - scheduled).num_minutes() as i32,
    })
}

fn build_geo_point(latitude: Option<f64>, longitude: Option<f64>) -> Option<GeoPoint> {
    Some(GeoPoint {
        latitude: latitude?,
        longitude: longitude?,
    })
}

fn parse_ojp_datetime(value: &str) -> Option<DateTime<FixedOffset>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Some(dt.with_timezone(&Zurich).fixed_offset());
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
        "StopPlace" | "StopPoint" | "Address" | "PointOfInterest" | "TopographicPlace"
    )
}

fn station_type_from_tag(tag: &str) -> StationType {
    match tag {
        "StopPlace" | "StopPoint" => StationType::Stop,
        "Address" => StationType::Address,
        "PointOfInterest" => StationType::Poi,
        "TopographicPlace" => StationType::Poi,
        _ => StationType::Unknown,
    }
}

fn is_location_result_container(stack: &[String], tag: &str) -> bool {
    tag == "Location"
        && stack.iter().rev().nth(1).is_some_and(|parent| {
            parent == "Location" || parent == "OJPLocationInformationDelivery"
        })
}

fn is_stop_call_container(tag: &str) -> bool {
    matches!(
        tag,
        "LegBoard"
            | "LegIntermediate"
            | "LegAlight"
            | "CallAtStop"
            | "IntermediateStop"
            | "OnwardCall"
            | "PreviousCall"
            | "LegStart"
            | "LegEnd"
    )
}

fn local_name(name: QName<'_>) -> String {
    let raw = String::from_utf8_lossy(name.as_ref());
    raw.split(':').next_back().unwrap_or_default().to_string()
}

fn path_contains(stack: &[String], candidates: &[&str]) -> bool {
    stack.iter().rev().take(4).any(|segment| {
        candidates
            .iter()
            .any(|candidate| segment == candidate || segment.ends_with(candidate))
    })
}

fn detect_platform_kind(stack: &[String], current: &str) -> Option<PlatformKind> {
    if is_platform_text_node(stack, current, "EstimatedQuay") {
        return Some(PlatformKind::Expected);
    }
    if is_platform_text_node(stack, current, "PlannedQuay") {
        return Some(PlatformKind::Scheduled);
    }
    if is_platform_text_node(stack, current, "Platform") {
        return Some(PlatformKind::Generic);
    }
    None
}

fn is_platform_text_node(stack: &[String], current: &str, container_tag: &str) -> bool {
    if current == container_tag {
        return true;
    }

    matches!(current, "Text" | "Name" | "Value") && path_contains(stack, &[container_tag])
}

fn set_platform_candidate(
    scheduled: &mut Option<String>,
    expected: &mut Option<String>,
    kind: PlatformKind,
    value: String,
) {
    let Some(platform) = normalize_platform_value(&value) else {
        return;
    };

    match kind {
        PlatformKind::Scheduled => {
            if scheduled.is_none() {
                *scheduled = Some(platform);
            }
        }
        PlatformKind::Expected => {
            if expected.is_none() {
                *expected = Some(platform);
            }
        }
        PlatformKind::Generic => {
            if expected.is_none() {
                *expected = Some(platform.clone());
            }
            if scheduled.is_none() {
                *scheduled = Some(platform);
            }
        }
    }
}

fn normalize_platform_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if looks_like_stop_ref(trimmed) {
        return platform_from_stop_point_ref(trimmed).or_else(|| Some(trimmed.to_string()));
    }

    Some(trimmed.to_string())
}

fn platform_from_stop_point_ref(value: &str) -> Option<String> {
    const IGNORED_SEGMENTS: &[&str] = &["ch", "ojp", "sloid", "sjyid", "plan"];

    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .rev()
        .find(|segment| {
            !segment.is_empty()
                && segment.len() <= 4
                && !IGNORED_SEGMENTS.contains(&segment.to_ascii_lowercase().as_str())
                && !segment.chars().all(|ch| ch.is_ascii_digit())
                && segment.chars().any(|ch| ch.is_ascii_alphabetic())
        })
        .map(ToString::to_string)
}

fn set_preferred_line(slot: &mut Option<String>, candidate: String) {
    if candidate.trim().is_empty() {
        return;
    }

    if looks_like_machine_identifier(&candidate) {
        return;
    }

    if slot
        .as_ref()
        .is_some_and(|current| !looks_like_machine_identifier(current))
    {
        return;
    }

    *slot = Some(candidate);
}

fn looks_like_machine_identifier(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("ojp:")
        || trimmed.starts_with("ch:")
        || (trimmed.contains(':') && trimmed.chars().any(|ch| ch.is_ascii_digit()))
}

fn now_fixed() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&Zurich).fixed_offset()
}

#[cfg(test)]
mod tests {
    use super::{parse_station_response, parse_trip_response, platform_from_stop_point_ref};

    #[test]
    fn parses_stop_call_platforms_from_nested_quay_text_and_stop_point_ref() {
        let xml = r#"
<OJP xmlns="http://www.vdv.de/ojp" xmlns:siri="http://www.siri.org.uk/siri" version="2.0">
  <OJPResponse>
    <siri:ServiceDelivery>
      <OJPTripDelivery>
        <TripResult>
          <Trip>
            <TripLeg>
              <TimedLeg>
                <Service>
                  <JourneyRef>trip-1</JourneyRef>
                  <Mode>bus</Mode>
                  <PublishedLineName>1</PublishedLineName>
                </Service>
                <LegBoard>
                  <StopPointRef>ojp:92001:A.</StopPointRef>
                  <StopPointName><Text>Kriens, Zentrum Pilatus</Text></StopPointName>
                  <PlannedQuay><Text>A</Text></PlannedQuay>
                  <ServiceDeparture>
                    <TimetabledTime>2026-04-27T10:57:00+02:00</TimetabledTime>
                    <EstimatedTime>2026-04-27T10:57:36+02:00</EstimatedTime>
                  </ServiceDeparture>
                </LegBoard>
                <LegAlight>
                  <StopPointRef>ojp:92002:B.</StopPointRef>
                  <StopPointName><Text>Luzern, Bahnhof</Text></StopPointName>
                  <ServiceArrival>
                    <TimetabledTime>2026-04-27T11:09:00+02:00</TimetabledTime>
                    <EstimatedTime>2026-04-27T11:09:30+02:00</EstimatedTime>
                  </ServiceArrival>
                </LegAlight>
              </TimedLeg>
            </TripLeg>
          </Trip>
        </TripResult>
      </OJPTripDelivery>
    </siri:ServiceDelivery>
  </OJPResponse>
</OJP>
"#;

        let trips = parse_trip_response(xml).expect("trip response should parse");
        assert_eq!(trips.len(), 1);
        assert_eq!(trips[0].legs.len(), 1);
        let leg = &trips[0].legs[0];

        assert_eq!(leg.from.platform.as_deref(), Some("A"));
        assert_eq!(leg.to.platform.as_deref(), Some("B"));
        assert_eq!(leg.stop_calls.len(), 2);
        assert_eq!(leg.stop_calls[0].platform.as_deref(), Some("A"));
        assert_eq!(leg.stop_calls[0].scheduled_platform.as_deref(), Some("A"));
        assert_eq!(leg.stop_calls[1].platform.as_deref(), Some("B"));
    }

    #[test]
    fn enriches_trip_coordinates_from_trip_response_context_places() {
        let xml = r#"
<OJP xmlns="http://www.vdv.de/ojp" xmlns:siri="http://www.siri.org.uk/siri" version="2.0">
  <OJPResponse>
    <siri:ServiceDelivery>
      <OJPTripDelivery>
        <TripResponseContext>
          <Places>
            <Place>
              <StopPoint>
                <siri:StopPointRef>ch:1:sloid:5000:4:8</siri:StopPointRef>
                <StopPointName><Text>Luzern</Text></StopPointName>
              </StopPoint>
              <Name><Text>Luzern</Text></Name>
              <GeoPosition>
                <siri:Longitude>8.31051</siri:Longitude>
                <siri:Latitude>47.04851</siri:Latitude>
              </GeoPosition>
            </Place>
            <Place>
              <StopPoint>
                <siri:StopPointRef>ch:1:sloid:7000:4:7</siri:StopPointRef>
                <StopPointName><Text>Bern</Text></StopPointName>
              </StopPoint>
              <Name><Text>Bern</Text></Name>
              <GeoPosition>
                <siri:Longitude>7.43683</siri:Longitude>
                <siri:Latitude>46.94857</siri:Latitude>
              </GeoPosition>
            </Place>
          </Places>
        </TripResponseContext>
        <TripResult>
          <Trip>
            <TripLeg>
              <TimedLeg>
                <Service>
                  <JourneyRef>trip-2</JourneyRef>
                  <Mode>rail</Mode>
                  <PublishedLineName>IR15</PublishedLineName>
                </Service>
                <LegBoard>
                  <siri:StopPointRef>ch:1:sloid:5000:4:8</siri:StopPointRef>
                  <StopPointName><Text>Luzern</Text></StopPointName>
                  <ServiceDeparture>
                    <TimetabledTime>2026-04-27T14:00:00+02:00</TimetabledTime>
                    <EstimatedTime>2026-04-27T14:00:00+02:00</EstimatedTime>
                  </ServiceDeparture>
                </LegBoard>
                <LegAlight>
                  <siri:StopPointRef>ch:1:sloid:7000:4:7</siri:StopPointRef>
                  <StopPointName><Text>Bern</Text></StopPointName>
                  <ServiceArrival>
                    <TimetabledTime>2026-04-27T15:00:00+02:00</TimetabledTime>
                    <EstimatedTime>2026-04-27T15:00:00+02:00</EstimatedTime>
                  </ServiceArrival>
                </LegAlight>
              </TimedLeg>
            </TripLeg>
          </Trip>
        </TripResult>
      </OJPTripDelivery>
    </siri:ServiceDelivery>
  </OJPResponse>
</OJP>
"#;

        let trips = parse_trip_response(xml).expect("trip response should parse");
        assert_eq!(trips.len(), 1);
        let leg = &trips[0].legs[0];
        assert_eq!(
            leg.from.coordinates.as_ref().map(|c| c.latitude),
            Some(47.04851)
        );
        assert_eq!(
            leg.from.coordinates.as_ref().map(|c| c.longitude),
            Some(8.31051)
        );
        assert_eq!(
            leg.to.coordinates.as_ref().map(|c| c.latitude),
            Some(46.94857)
        );
        assert_eq!(
            leg.to.coordinates.as_ref().map(|c| c.longitude),
            Some(7.43683)
        );
        assert_eq!(
            leg.stop_calls[0].coordinates.as_ref().map(|c| c.latitude),
            Some(47.04851)
        );
        assert_eq!(
            leg.stop_calls[1].coordinates.as_ref().map(|c| c.latitude),
            Some(46.94857)
        );
    }

    #[test]
    fn keeps_boarding_occupancy_when_intermediate_calls_differ() {
        let xml = r#"
<OJP xmlns="http://www.vdv.de/ojp" xmlns:siri="http://www.siri.org.uk/siri" version="2.0">
  <OJPResponse>
    <siri:ServiceDelivery>
      <OJPTripDelivery>
        <TripResult>
          <Trip>
            <TripLeg>
              <TimedLeg>
                <Service>
                  <JourneyRef>trip-3</JourneyRef>
                  <Mode>rail</Mode>
                  <PublishedLineName>IR75</PublishedLineName>
                </Service>
                <LegBoard>
                  <StopPointRef>ch:1:sloid:5000:4:9</StopPointRef>
                  <StopPointName><Text>Luzern</Text></StopPointName>
                  <ServiceDeparture>
                    <TimetabledTime>2026-04-28T07:35:00+02:00</TimetabledTime>
                    <EstimatedTime>2026-04-28T07:35:00+02:00</EstimatedTime>
                  </ServiceDeparture>
                  <siri:ExpectedDepartureOccupancy>
                    <siri:FareClass>firstClass</siri:FareClass>
                    <siri:OccupancyLevel>fewSeatsAvailable</siri:OccupancyLevel>
                  </siri:ExpectedDepartureOccupancy>
                  <siri:ExpectedDepartureOccupancy>
                    <siri:FareClass>secondClass</siri:FareClass>
                    <siri:OccupancyLevel>standingRoomOnly</siri:OccupancyLevel>
                  </siri:ExpectedDepartureOccupancy>
                </LegBoard>
                <LegIntermediate>
                  <StopPointRef>ch:1:sloid:7000:4:7</StopPointRef>
                  <StopPointName><Text>Bern</Text></StopPointName>
                  <ServiceArrival>
                    <TimetabledTime>2026-04-28T08:10:00+02:00</TimetabledTime>
                    <EstimatedTime>2026-04-28T08:10:00+02:00</EstimatedTime>
                  </ServiceArrival>
                  <ServiceDeparture>
                    <TimetabledTime>2026-04-28T08:11:00+02:00</TimetabledTime>
                    <EstimatedTime>2026-04-28T08:11:00+02:00</EstimatedTime>
                  </ServiceDeparture>
                  <siri:ExpectedDepartureOccupancy>
                    <siri:FareClass>firstClass</siri:FareClass>
                    <siri:OccupancyLevel>manySeatsAvailable</siri:OccupancyLevel>
                  </siri:ExpectedDepartureOccupancy>
                  <siri:ExpectedDepartureOccupancy>
                    <siri:FareClass>secondClass</siri:FareClass>
                    <siri:OccupancyLevel>manySeatsAvailable</siri:OccupancyLevel>
                  </siri:ExpectedDepartureOccupancy>
                </LegIntermediate>
                <LegAlight>
                  <StopPointRef>ch:1:sloid:8503000:4:1</StopPointRef>
                  <StopPointName><Text>Zürich HB</Text></StopPointName>
                  <ServiceArrival>
                    <TimetabledTime>2026-04-28T08:25:00+02:00</TimetabledTime>
                    <EstimatedTime>2026-04-28T08:25:00+02:00</EstimatedTime>
                  </ServiceArrival>
                </LegAlight>
              </TimedLeg>
            </TripLeg>
          </Trip>
        </TripResult>
      </OJPTripDelivery>
    </siri:ServiceDelivery>
  </OJPResponse>
</OJP>
"#;

        let trips = parse_trip_response(xml).expect("trip response should parse");
        assert_eq!(trips.len(), 1);
        let occupancy = trips[0].legs[0]
            .occupancy
            .as_ref()
            .expect("occupancy should be present");
        assert_eq!(occupancy.first_class.as_deref(), Some("high occupancy"));
        assert_eq!(
            occupancy.second_class.as_deref(),
            Some("very high occupancy")
        );
        assert_eq!(occupancy.general.as_deref(), Some("very high occupancy"));
    }

    #[test]
    fn ignores_namespace_like_stop_point_ref_segments() {
        assert_eq!(platform_from_stop_point_ref("ch:1:sloid:8589725"), None);
        assert_eq!(
            platform_from_stop_point_ref("ojp:92001:A."),
            Some("A".to_string())
        );
    }

    #[test]
    fn parses_station_coordinates_from_location_container_geo_position() {
        let xml = r#"
<OJP xmlns="http://www.vdv.de/ojp" xmlns:siri="http://www.siri.org.uk/siri" version="2.0">
  <OJPResponse>
    <siri:ServiceDelivery>
      <OJPLocationInformationDelivery>
        <Location>
          <Location>
            <StopPlace>
              <StopPlaceRef>8503000</StopPlaceRef>
              <StopPlaceName><Text>Zürich HB</Text></StopPlaceName>
            </StopPlace>
            <LocationName><Text>Zürich HB</Text></LocationName>
            <GeoPosition>
              <siri:Longitude>8.540192</siri:Longitude>
              <siri:Latitude>47.378177</siri:Latitude>
            </GeoPosition>
          </Location>
        </Location>
      </OJPLocationInformationDelivery>
    </siri:ServiceDelivery>
  </OJPResponse>
</OJP>
"#;

        let stations = parse_station_response(xml, 5).expect("station response should parse");
        assert_eq!(stations.len(), 1);
        assert_eq!(stations[0].id, "8503000");
        assert_eq!(stations[0].name, "Zürich HB");
        assert_eq!(stations[0].longitude, Some(8.540192));
        assert_eq!(stations[0].latitude, Some(47.378177));
    }

    #[test]
    fn parses_station_coordinates_from_single_location_container() {
        let xml = r#"
<OJP xmlns="http://www.vdv.de/ojp" xmlns:siri="http://www.siri.org.uk/siri" version="2.0">
  <OJPResponse>
    <siri:ServiceDelivery>
      <OJPLocationInformationDelivery>
        <Location>
          <StopPlace>
            <StopPlaceRef>8505000</StopPlaceRef>
            <StopPlaceName><Text>Zug</Text></StopPlaceName>
          </StopPlace>
          <LocationName><Text>Zug</Text></LocationName>
          <GeoPosition>
            <siri:Longitude>8.515494</siri:Longitude>
            <siri:Latitude>47.177770</siri:Latitude>
          </GeoPosition>
        </Location>
      </OJPLocationInformationDelivery>
    </siri:ServiceDelivery>
  </OJPResponse>
</OJP>
"#;

        let stations = parse_station_response(xml, 5).expect("station response should parse");
        assert_eq!(stations.len(), 1);
        assert_eq!(stations[0].id, "8505000");
        assert_eq!(stations[0].name, "Zug");
        assert_eq!(stations[0].longitude, Some(8.515494));
        assert_eq!(stations[0].latitude, Some(47.17777));
    }
}
