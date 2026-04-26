#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FeedMessage {
    #[prost(message, optional, tag = "1")]
    pub header: Option<FeedHeader>,
    #[prost(message, repeated, tag = "2")]
    pub entity: Vec<FeedEntity>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FeedHeader {
    #[prost(string, optional, tag = "1")]
    pub gtfs_realtime_version: Option<String>,
    #[prost(enumeration = "Incrementality", optional, tag = "2")]
    pub incrementality: Option<i32>,
    #[prost(uint64, optional, tag = "3")]
    pub timestamp: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ::prost::Enumeration)]
#[repr(i32)]
pub enum Incrementality {
    FullDataset = 0,
    Differential = 1,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FeedEntity {
    #[prost(string, optional, tag = "1")]
    pub id: Option<String>,
    #[prost(bool, optional, tag = "2")]
    pub is_deleted: Option<bool>,
    #[prost(message, optional, tag = "3")]
    pub trip_update: Option<TripUpdate>,
    #[prost(message, optional, tag = "4")]
    pub vehicle: Option<VehiclePosition>,
    #[prost(message, optional, tag = "5")]
    pub alert: Option<Alert>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TripUpdate {
    #[prost(message, optional, tag = "1")]
    pub trip: Option<TripDescriptor>,
    #[prost(message, optional, tag = "2")]
    pub vehicle: Option<VehicleDescriptor>,
    #[prost(message, repeated, tag = "3")]
    pub stop_time_update: Vec<StopTimeUpdate>,
    #[prost(uint64, optional, tag = "4")]
    pub timestamp: Option<u64>,
    #[prost(uint32, optional, tag = "5")]
    pub delay: Option<u32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TripDescriptor {
    #[prost(string, optional, tag = "1")]
    pub trip_id: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub route_id: Option<String>,
    #[prost(uint32, optional, tag = "3")]
    pub direction_id: Option<u32>,
    #[prost(string, optional, tag = "4")]
    pub start_time: Option<String>,
    #[prost(string, optional, tag = "5")]
    pub start_date: Option<String>,
    #[prost(
        enumeration = "trip_descriptor::ScheduleRelationship",
        optional,
        tag = "6"
    )]
    pub schedule_relationship: Option<i32>,
}

pub mod trip_descriptor {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum ScheduleRelationship {
        Scheduled = 0,
        Added = 1,
        Unscheduled = 2,
        Canceled = 3,
        Replaced = 5,
        Duplicated = 6,
        Deleted = 7,
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct VehicleDescriptor {
    #[prost(string, optional, tag = "1")]
    pub id: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub label: Option<String>,
    #[prost(string, optional, tag = "3")]
    pub license_plate: Option<String>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct StopTimeEvent {
    #[prost(int32, optional, tag = "1")]
    pub delay: Option<i32>,
    #[prost(int64, optional, tag = "2")]
    pub time: Option<i64>,
    #[prost(int32, optional, tag = "3")]
    pub uncertainty: Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct StopTimeUpdate {
    #[prost(uint32, optional, tag = "1")]
    pub stop_sequence: Option<u32>,
    #[prost(string, optional, tag = "4")]
    pub stop_id: Option<String>,
    #[prost(message, optional, tag = "2")]
    pub arrival: Option<StopTimeEvent>,
    #[prost(message, optional, tag = "3")]
    pub departure: Option<StopTimeEvent>,
    #[prost(
        enumeration = "stop_time_update::ScheduleRelationship",
        optional,
        tag = "5"
    )]
    pub schedule_relationship: Option<i32>,
    #[prost(string, optional, tag = "6")]
    pub stop_headsign: Option<String>,
}

pub mod stop_time_update {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum ScheduleRelationship {
        Scheduled = 0,
        Skipped = 1,
        NoData = 2,
        Unscheduled = 3,
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct VehiclePosition {}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Alert {
    #[prost(message, repeated, tag = "1")]
    pub active_period: Vec<TimeRange>,
    #[prost(message, repeated, tag = "5")]
    pub informed_entity: Vec<EntitySelector>,
    #[prost(enumeration = "alert::Cause", optional, tag = "6")]
    pub cause: Option<i32>,
    #[prost(enumeration = "alert::Effect", optional, tag = "7")]
    pub effect: Option<i32>,
    #[prost(message, optional, tag = "8")]
    pub url: Option<TranslatedString>,
    #[prost(message, optional, tag = "10")]
    pub header_text: Option<TranslatedString>,
    #[prost(message, optional, tag = "11")]
    pub description_text: Option<TranslatedString>,
}

pub mod alert {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum Cause {
        UnknownCause = 1,
        OtherCause = 2,
        TechnicalProblem = 3,
        Strike = 4,
        Demonstration = 5,
        Accident = 6,
        Holiday = 7,
        Weather = 8,
        Maintenance = 9,
        Construction = 10,
        PoliceActivity = 11,
        MedicalEmergency = 12,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum Effect {
        NoService = 1,
        ReducedService = 2,
        SignificantDelays = 3,
        Detour = 4,
        AdditionalService = 5,
        ModifiedService = 6,
        OtherEffect = 7,
        UnknownEffect = 8,
        StopMoved = 9,
        NoEffect = 10,
        AccessibilityIssue = 11,
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TimeRange {
    #[prost(uint64, optional, tag = "1")]
    pub start: Option<u64>,
    #[prost(uint64, optional, tag = "2")]
    pub end: Option<u64>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct EntitySelector {
    #[prost(string, optional, tag = "1")]
    pub agency_id: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub route_id: Option<String>,
    #[prost(int32, optional, tag = "3")]
    pub route_type: Option<i32>,
    #[prost(message, optional, tag = "4")]
    pub trip: Option<TripDescriptor>,
    #[prost(string, optional, tag = "5")]
    pub stop_id: Option<String>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TranslatedString {
    #[prost(message, repeated, tag = "1")]
    pub translation: Vec<translated_string::Translation>,
}

pub mod translated_string {
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Translation {
        #[prost(string, optional, tag = "1")]
        pub text: Option<String>,
        #[prost(string, optional, tag = "2")]
        pub language: Option<String>,
    }
}
