use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Station {
    pub id: String,
    pub name: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub station_type: StationType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StationType {
    Stop,
    Address,
    Poi,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportMode {
    Rail,
    Bus,
    Tram,
    Ship,
    Cableway,
    Funicular,
    All,
}

impl TransportMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rail => "rail",
            Self::Bus => "bus",
            Self::Tram => "tram",
            Self::Ship => "ship",
            Self::Cableway => "cableway",
            Self::Funicular => "funicular",
            Self::All => "all",
        }
    }

    pub fn all_modes() -> Vec<Self> {
        vec![
            Self::Rail,
            Self::Bus,
            Self::Tram,
            Self::Ship,
            Self::Cableway,
            Self::Funicular,
            Self::All,
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trip {
    pub id: String,
    pub duration_minutes: i64,
    pub legs: Vec<TripLeg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripLeg {
    pub mode: String,
    pub line: Option<String>,
    pub service: String,
    pub operator: Option<String>,
    pub occupancy: Option<TripLegOccupancy>,
    pub stops: Vec<String>,
    pub stop_calls: Vec<TripLegStopCall>,
    pub from: LegStop,
    pub to: LegStop,
    pub departure: TimeInfo,
    pub arrival: TimeInfo,
    pub status: TripStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegStop {
    pub station: String,
    pub platform: Option<String>,
    pub coordinates: Option<GeoPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeInfo {
    pub scheduled: DateTime<FixedOffset>,
    pub expected: DateTime<FixedOffset>,
    pub delay_minutes: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripLegStopCall {
    pub station: String,
    pub platform: Option<String>,
    pub scheduled_platform: Option<String>,
    pub expected_platform: Option<String>,
    pub coordinates: Option<GeoPoint>,
    pub arrival: Option<StopCallTime>,
    pub departure: Option<StopCallTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoPoint {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopCallTime {
    pub scheduled: DateTime<FixedOffset>,
    pub expected: DateTime<FixedOffset>,
    pub delay_minutes: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripLegOccupancy {
    pub first_class: Option<String>,
    pub second_class: Option<String>,
    pub general: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Departure {
    pub trip_id: Option<String>,
    pub line: Option<String>,
    pub service: String,
    pub destination: String,
    pub mode: Option<String>,
    pub scheduled: DateTime<FixedOffset>,
    pub expected: DateTime<FixedOffset>,
    pub delay_minutes: i32,
    pub platform: Option<String>,
    pub status: TripStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripDetails {
    pub trip_id: String,
    pub current_delay_minutes: i32,
    pub is_cancelled: bool,
    pub platform_changes: Vec<PlatformChange>,
    pub stop_updates: Vec<StopUpdate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformChange {
    pub stop_id: Option<String>,
    pub stop_name: Option<String>,
    pub scheduled_platform: Option<String>,
    pub expected_platform: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopUpdate {
    pub stop_id: Option<String>,
    pub stop_name: Option<String>,
    pub arrival_delay_minutes: Option<i32>,
    pub departure_delay_minutes: Option<i32>,
    pub schedule_relationship: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disruption {
    pub id: String,
    pub affected_lines: Vec<String>,
    pub affected_stations: Vec<String>,
    pub reason: Option<String>,
    pub description: Option<String>,
    pub starts_at: Option<DateTime<FixedOffset>>,
    pub ends_at: Option<DateTime<FixedOffset>>,
    pub alternatives: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorSubscription {
    pub subscription_id: String,
    pub trip_id: String,
    pub notify_on_delay_minutes: i32,
    pub created_at: DateTime<FixedOffset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolWarning {
    pub message: String,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripStatus {
    OnTime,
    Delayed,
    Cancelled,
    Redirected,
    PlatformChanged,
}

impl TripStatus {
    pub fn from_delay(delay_minutes: i32) -> Self {
        if delay_minutes > 0 {
            Self::Delayed
        } else {
            Self::OnTime
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StationsResponse {
    pub stations: Vec<Station>,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripsResponse {
    pub trips: Vec<Trip>,
    pub warning: Option<ToolWarning>,
    pub report: Option<TripPlanReport>,
    pub option_reports: Vec<TripOptionReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeparturesResponse {
    pub departures: Vec<Departure>,
    pub warning: Option<ToolWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripDetailsResponse {
    pub trip: TripDetails,
    pub warning: Option<ToolWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisruptionsResponse {
    pub disruptions: Vec<Disruption>,
    pub warning: Option<ToolWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripPlanReport {
    pub generated_at: DateTime<FixedOffset>,
    pub origin: String,
    pub destination: String,
    pub total_options: usize,
    pub delayed_options: usize,
    pub live_events: Vec<Disruption>,
    pub live_events_status: String,
    pub live_events_warning: Option<ToolWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripOptionReport {
    pub trip_id: String,
    pub time: TripOptionTime,
    pub platform: TripOptionPlatform,
    pub final_destination: String,
    pub origin_coordinates: Option<GeoPoint>,
    pub final_destination_coordinates: Option<GeoPoint>,
    pub occupancy: Option<TripLegOccupancy>,
    pub events: Vec<Disruption>,
    pub events_status: String,
    pub wagon_formation: Option<String>,
    pub wagon_formation_status: String,
    pub wagon_formation_readable: Option<String>,
    pub wagon_formation_diagram: Option<String>,
    pub wagon_formation_human_display: Option<String>,
    pub wagon_formation_boarding_hint: Option<String>,
    pub wagon_formation_legend: Option<Vec<String>>,
    pub wagon_formation_parsed: Option<WagonFormationParsed>,
    pub sector_to_coach_range: Option<Vec<WagonFormationSectorRange>>,
    pub accessible_coaches: Option<Vec<String>>,
    pub first_class_coaches: Option<Vec<String>>,
    pub second_class_coaches: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripOptionTime {
    pub departure: DateTime<FixedOffset>,
    pub arrival: DateTime<FixedOffset>,
    pub duration_minutes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripOptionPlatform {
    pub departure: Option<String>,
    pub arrival: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WagonFormationParsed {
    pub coaches: Vec<WagonFormationCoach>,
    pub padding: WagonFormationPadding,
    pub sectors_seen: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WagonFormationCoach {
    pub id: String,
    pub vehicle_type: String,
    pub class_label: String,
    pub sector: Option<String>,
    pub services: Vec<String>,
    pub no_passage_prev: bool,
    pub no_passage_next: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WagonFormationPadding {
    pub front: usize,
    pub rear: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WagonFormationSectorRange {
    pub sector: String,
    pub from_coach: String,
    pub to_coach: String,
}

pub fn format_service_label(mode: Option<&str>, line: Option<&str>) -> String {
    let mode_label = mode
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| match value.to_ascii_lowercase().as_str() {
            "rail" | "train" => "Train",
            "bus" => "Bus",
            "tram" => "Tram",
            "ship" | "water" => "Boat",
            "cableway" => "Cableway",
            "funicular" => "Funicular",
            _ => "Transit",
        })
        .unwrap_or("Transit");

    match line.map(str::trim).filter(|value| !value.is_empty()) {
        Some(line) => format!("{mode_label} {line}"),
        None => mode_label.to_string(),
    }
}
