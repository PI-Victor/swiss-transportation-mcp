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
    pub operator: Option<String>,
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
pub struct Departure {
    pub trip_id: Option<String>,
    pub line: Option<String>,
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
