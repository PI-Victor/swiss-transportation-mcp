use chrono::{
    DateTime, Datelike, FixedOffset, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc,
};
use chrono_tz::Europe::Zurich;
use regex::Regex;

use anyhow::{Result, anyhow};

use crate::models::TransportMode;

pub fn parse_datetime_input(input: Option<&str>) -> Result<DateTime<FixedOffset>> {
    let now = Utc::now().with_timezone(&Zurich);
    let Some(value) = input.map(|v| v.trim()).filter(|v| !v.is_empty()) else {
        return Ok(now.fixed_offset());
    };

    let lower = value.to_ascii_lowercase();
    if lower == "earliest" || lower == "now" {
        return Ok(now.fixed_offset());
    }

    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed);
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
        let naive = tomorrow.and_time(parsed_time);
        return map_zurich_naive(naive);
    }

    Err(anyhow!("unsupported datetime value: {value}"))
}

pub fn parse_modes(modes: Option<Vec<String>>, free_text: Option<&str>) -> Vec<TransportMode> {
    if let Some(explicit) = modes {
        let parsed: Vec<_> = explicit
            .into_iter()
            .filter_map(|mode| parse_mode_token(&mode))
            .collect();
        if !parsed.is_empty() {
            return dedupe_modes(parsed);
        }
    }

    if let Some(text) = free_text {
        let lower = text.to_ascii_lowercase();
        let mut parsed = Vec::new();

        if lower.contains("train") || lower.contains("rail") {
            parsed.push(TransportMode::Rail);
        }
        if lower.contains("bus") {
            parsed.push(TransportMode::Bus);
        }
        if lower.contains("tram") {
            parsed.push(TransportMode::Tram);
        }
        if lower.contains("boat") || lower.contains("ship") {
            parsed.push(TransportMode::Ship);
        }
        if lower.contains("cable") || lower.contains("gondola") {
            parsed.push(TransportMode::Cableway);
        }
        if lower.contains("funicular") || lower.contains("standseilbahn") {
            parsed.push(TransportMode::Funicular);
        }

        if !parsed.is_empty() {
            return dedupe_modes(parsed);
        }
    }

    vec![TransportMode::All]
}

pub fn split_route_phrase(from_station: &str, to_station: Option<&str>) -> (String, String) {
    if let Some(to) = to_station.filter(|value| !value.trim().is_empty()) {
        return (from_station.trim().to_string(), to.trim().to_string());
    }

    let lower = from_station.to_ascii_lowercase();
    if let Some(index) = lower.find(" to ") {
        let from = from_station[..index].trim().to_string();
        let to = from_station[(index + 4)..].trim().to_string();
        if !from.is_empty() && !to.is_empty() {
            return (from, to);
        }
    }

    (from_station.trim().to_string(), String::new())
}

fn parse_mode_token(token: &str) -> Option<TransportMode> {
    match token.trim().to_ascii_lowercase().as_str() {
        "rail" | "train" => Some(TransportMode::Rail),
        "bus" => Some(TransportMode::Bus),
        "tram" => Some(TransportMode::Tram),
        "ship" | "boat" => Some(TransportMode::Ship),
        "cableway" | "cable" => Some(TransportMode::Cableway),
        "funicular" | "standseilbahn" => Some(TransportMode::Funicular),
        "all" => Some(TransportMode::All),
        _ => None,
    }
}

fn dedupe_modes(modes: Vec<TransportMode>) -> Vec<TransportMode> {
    let mut out = Vec::new();
    for mode in modes {
        if !out
            .iter()
            .any(|existing: &TransportMode| existing.as_str() == mode.as_str())
        {
            out.push(mode);
        }
    }
    out
}

fn parse_hour_minute(text: &str) -> Option<NaiveTime> {
    let regex = Regex::new(r"(?i)(\d{1,2})(?::(\d{2}))?").ok()?;
    let capture = regex.captures(text)?;
    let hour = capture.get(1)?.as_str().parse::<u32>().ok()?;
    let minute = capture
        .get(2)
        .and_then(|v| v.as_str().parse::<u32>().ok())
        .unwrap_or(0);
    NaiveTime::from_hms_opt(hour.min(23), minute.min(59), 0)
}

fn map_zurich_naive(value: NaiveDateTime) -> Result<DateTime<FixedOffset>> {
    match Zurich.from_local_datetime(&value) {
        LocalResult::Single(datetime) => Ok(datetime.fixed_offset()),
        LocalResult::Ambiguous(first, _) => Ok(first.fixed_offset()),
        LocalResult::None => Err(anyhow!(
            "datetime is invalid in Europe/Zurich timezone: {}-{:02}-{:02} {}",
            value.year(),
            value.month(),
            value.day(),
            value.time()
        )),
    }
}
