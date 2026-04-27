use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use chrono::{DateTime, FixedOffset, Utc};
use chrono_tz::Europe::Zurich;
use regex::Regex;
use serde_json::Value;

use crate::models::{
    Trip, WagonFormationCoach, WagonFormationPadding, WagonFormationParsed,
    WagonFormationSectorRange,
};

#[derive(Clone)]
pub struct FormationClient {
    http: reqwest::Client,
    endpoint: String,
    token: String,
}

#[derive(Debug, Clone)]
pub struct FormationRender {
    pub readable: String,
    pub diagram: String,
    pub human_display: String,
    pub boarding_hint: String,
    pub legend: Vec<String>,
    pub parsed: WagonFormationParsed,
    pub sector_to_coach_range: Vec<WagonFormationSectorRange>,
    pub accessible_coaches: Vec<String>,
    pub first_class_coaches: Vec<String>,
    pub second_class_coaches: Vec<String>,
}

#[derive(Debug, Clone)]
struct FormationCoach {
    type_code: String,
    order: Option<String>,
    services: Vec<String>,
    sector: Option<char>,
    no_pass_left: bool,
    no_pass_right: bool,
    is_padding: bool,
}

impl FormationClient {
    pub fn new(endpoint: String, token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoint,
            token,
        }
    }

    pub async fn wagon_formation_for_trip(&self, trip: &Trip) -> Result<Option<String>> {
        let Some(first_rail_leg) = trip
            .legs
            .iter()
            .find(|leg| matches!(leg.mode.to_ascii_lowercase().as_str(), "rail" | "train"))
        else {
            return Ok(None);
        };

        let operation_date = first_rail_leg.departure.expected.date_naive();
        let evu = infer_evu(trip).unwrap_or("SBBP");
        let candidates = extract_train_number_candidates(trip);
        if candidates.is_empty() {
            return Ok(None);
        }
        let endpoints = formation_endpoint_candidates(&self.endpoint, operation_date);

        for endpoint in endpoints {
            for candidate in &candidates {
                let response = self
                    .http
                    .get(&endpoint)
                    .bearer_auth(&self.token)
                    .query(&[
                        ("evu", evu),
                        (
                            "operationDate",
                            &operation_date.format("%Y-%m-%d").to_string(),
                        ),
                        ("trainNumber", candidate.as_str()),
                    ])
                    .send()
                    .await?;

                let status = response.status();
                if status == reqwest::StatusCode::NOT_FOUND
                    || status == reqwest::StatusCode::BAD_REQUEST
                {
                    continue;
                }

                if !status.is_success() {
                    return Err(anyhow!("formation API returned status {}", status));
                }

                let payload: Value = serde_json::from_str(&response.text().await?)?;
                if let Some(formation) = find_best_stop_formation_short_string(
                    &payload,
                    &first_rail_leg.from.station,
                    first_rail_leg.departure.scheduled,
                ) {
                    return Ok(Some(formation));
                }
                if let Some(formation) = find_first_formation_short_string(&payload) {
                    return Ok(Some(formation));
                }
            }
        }

        Ok(None)
    }
}

fn find_best_stop_formation_short_string(
    payload: &Value,
    trip_departure_station: &str,
    trip_departure_time: DateTime<FixedOffset>,
) -> Option<String> {
    let stops = payload
        .get("formationsAtScheduledStops")
        .and_then(Value::as_array)?;

    let target_station = normalize_station_name(trip_departure_station);

    let mut best_station_time_match: Option<(i64, String)> = None;
    let mut best_station_only_match: Option<String> = None;
    let mut first_available: Option<String> = None;

    for stop in stops {
        let station_name = stop
            .get("scheduledStop")
            .and_then(Value::as_object)
            .and_then(|scheduled| scheduled.get("stopPoint"))
            .and_then(Value::as_object)
            .and_then(|stop_point| stop_point.get("name"))
            .and_then(Value::as_str)
            .map(normalize_station_name);

        let formation = stop
            .get("formationShort")
            .and_then(Value::as_object)
            .and_then(|formation_short| formation_short.get("formationShortString"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        let Some(formation) = formation else {
            continue;
        };

        if first_available.is_none() {
            first_available = Some(formation.clone());
        }

        let Some(station_name) = station_name else {
            continue;
        };
        if station_name != target_station {
            continue;
        }

        let departure_time = stop
            .get("scheduledStop")
            .and_then(Value::as_object)
            .and_then(|scheduled| scheduled.get("stopTime"))
            .and_then(Value::as_object)
            .and_then(|stop_time| stop_time.get("departureTime"))
            .and_then(Value::as_str)
            .and_then(|time| DateTime::parse_from_rfc3339(time).ok());

        if let Some(departure_time) = departure_time {
            let delta = (departure_time - trip_departure_time).num_seconds().abs();
            match &best_station_time_match {
                Some((current_delta, _)) if *current_delta <= delta => {}
                _ => {
                    best_station_time_match = Some((delta, formation.clone()));
                }
            }
        }

        if best_station_only_match.is_none() {
            best_station_only_match = Some(formation);
        }
    }

    best_station_time_match
        .map(|(_, formation)| formation)
        .or(best_station_only_match)
        .or(first_available)
}

pub fn render_formation_short_string(short: &str) -> Option<FormationRender> {
    let short = short.trim();
    if short.is_empty() {
        return None;
    }

    let mut coaches = Vec::new();
    let mut sector_markers = Vec::new();
    let mut pending_sector: Option<char> = None;

    for raw_part in short.split(',') {
        let part = raw_part.trim();
        if part.is_empty() {
            continue;
        }

        let unwrapped = part.replace(['[', ']'], "");
        let (without_markers, markers) = strip_sector_markers(&unwrapped);
        sector_markers.extend(markers.iter().copied());
        if let Some(marker) = markers.last().copied() {
            pending_sector = Some(marker);
        }

        let token = without_markers.trim();
        if token.is_empty() {
            continue;
        }

        if let Some(mut coach) = parse_coach_token(token) {
            if coach.sector.is_none() {
                coach.sector = pending_sector.take();
            }
            coaches.push(coach);
        }
    }

    let first_real = coaches.iter().position(|coach| !coach.is_padding)?;
    let last_real = coaches.iter().rposition(|coach| !coach.is_padding)?;
    let front_padding = coaches[..first_real]
        .iter()
        .filter(|coach| coach.is_padding)
        .count();
    let rear_padding = coaches[last_real + 1..]
        .iter()
        .filter(|coach| coach.is_padding)
        .count();

    let real_coaches: Vec<_> = coaches[first_real..=last_real]
        .iter()
        .filter(|coach| !coach.is_padding)
        .cloned()
        .collect();
    if real_coaches.is_empty() {
        return None;
    }

    let readable = render_readable_summary(&real_coaches, &sector_markers, front_padding, rear_padding);
    let diagram = render_diagram(&real_coaches);
    let legend = render_legend(&real_coaches);
    let parsed = build_parsed_summary(&real_coaches, &sector_markers, front_padding, rear_padding);
    let sector_to_coach_range = build_sector_ranges(&real_coaches);
    let accessible_coaches = build_accessible_coach_list(&real_coaches);
    let first_class_coaches = build_class_coach_list(&real_coaches, coach_has_first_class);
    let second_class_coaches = build_class_coach_list(&real_coaches, coach_has_second_class);
    let boarding_hint =
        build_boarding_hint(&parsed, &accessible_coaches).unwrap_or_else(|| "Use sector markers shown for your coach.".to_string());
    let human_display = render_human_display(
        &parsed,
        &sector_to_coach_range,
        &accessible_coaches,
        &first_class_coaches,
        &second_class_coaches,
        &boarding_hint,
        &diagram,
    );

    Some(FormationRender {
        readable,
        diagram,
        human_display,
        boarding_hint,
        legend,
        parsed,
        sector_to_coach_range,
        accessible_coaches,
        first_class_coaches,
        second_class_coaches,
    })
}

fn formation_endpoint_candidates(
    base_endpoint: &str,
    operation_date: chrono::NaiveDate,
) -> Vec<String> {
    let today = Utc::now().with_timezone(&Zurich).date_naive();
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    let mut push_unique = |value: String| {
        if seen.insert(value.clone()) {
            candidates.push(value);
        }
    };

    // The provider documents stop_based as "today only"; future dates should use vehicle/full.
    if operation_date > today && base_endpoint.contains("formations_stop_based") {
        push_unique(base_endpoint.replace("formations_stop_based", "formations_vehicle_based"));
        push_unique(base_endpoint.replace("formations_stop_based", "formations_full"));
    }

    push_unique(base_endpoint.to_string());
    candidates
}

fn parse_coach_token(token: &str) -> Option<FormationCoach> {
    let no_pass_left = token.contains('(');
    let no_pass_right = token.contains(')');

    let core = token.replace(['(', ')'], "");
    let core = core.trim();
    if core.is_empty() {
        return None;
    }

    let (vehicle_part, services_part) = match core.split_once('#') {
        Some((vehicle, services)) => (vehicle.trim(), Some(services.trim())),
        None => (core, None),
    };

    let (type_code, order) = match vehicle_part.split_once(':') {
        Some((vehicle_type, ord)) => (vehicle_type.trim(), Some(ord.trim().to_string())),
        None => (vehicle_part.trim(), None),
    };
    if type_code.is_empty() {
        return None;
    }

    let services = services_part
        .map(|raw| {
            raw.split(';')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(FormationCoach {
        type_code: type_code.to_string(),
        order,
        services,
        sector: None,
        no_pass_left,
        no_pass_right,
        is_padding: type_code.eq_ignore_ascii_case("F"),
    })
}

fn strip_sector_markers(token: &str) -> (String, Vec<char>) {
    let mut cleaned = String::with_capacity(token.len());
    let mut sectors = Vec::new();
    let mut chars = token.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '@' {
            if let Some(marker) = chars.peek().copied()
                && marker.is_ascii_alphabetic()
            {
                sectors.push(marker.to_ascii_uppercase());
                chars.next();
                continue;
            }
        }
        cleaned.push(ch);
    }

    (cleaned, sectors)
}

fn render_readable_summary(
    real_coaches: &[FormationCoach],
    sector_markers: &[char],
    front_padding: usize,
    rear_padding: usize,
) -> String {
    let sector_text = if sector_markers.is_empty() {
        String::new()
    } else {
        let mut deduped = Vec::new();
        let mut seen = HashSet::new();
        for marker in sector_markers {
            if seen.insert(*marker) {
                deduped.push(*marker);
            }
        }
        format!(
            "Platform sectors seen: {}. ",
            deduped
                .iter()
                .map(char::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let coach_text = real_coaches
        .iter()
        .map(|coach| {
            let mut details = Vec::new();
            details.push(map_vehicle_type(&coach.type_code).to_string());
            if let Some(sector) = coach.sector {
                details.push(format!("sector {sector}"));
            }
            if coach.no_pass_left {
                details.push("no passage to previous coach".to_string());
            }
            if coach.no_pass_right {
                details.push("no passage to next coach".to_string());
            }
            for service in &coach.services {
                details.push(service_description(service));
            }

            let id = coach_label(coach);
            format!("{id} ({})", details.join(", "))
        })
        .collect::<Vec<_>>()
        .join(" -> ");

    let padding_text = format!("Padding coaches: front {front_padding}, rear {rear_padding}.");

    format!("{sector_text}Coaches front->rear: {coach_text}. {padding_text}")
}

fn render_diagram(real_coaches: &[FormationCoach]) -> String {
    real_coaches
        .iter()
        .map(|coach| {
            let mut label = coach_label(coach);
            if !coach.services.is_empty() {
                label.push(' ');
                label.push_str(&coach.services.join("+"));
            }
            if let Some(sector) = coach.sector {
                label.push_str(&format!(" @{sector}"));
            }
            let mut token = format!("|{label}|");
            if coach.no_pass_left {
                token = format!("({token}");
            }
            if coach.no_pass_right {
                token = format!("{token})");
            }
            token
        })
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn coach_label(coach: &FormationCoach) -> String {
    match &coach.order {
        Some(order) => format!("{}:{order}", coach.type_code),
        None => coach.type_code.clone(),
    }
}

fn render_legend(real_coaches: &[FormationCoach]) -> Vec<String> {
    let mut legend = vec![
        "-> = next coach toward rear".to_string(),
        "( ... ) = no passage at one end of the marked coach".to_string(),
    ];

    if real_coaches.iter().any(|coach| coach.sector.is_some()) {
        legend.push("@X = platform sector marker near that coach (X = A/B/C/...)".to_string());
    }

    let mut seen_types = HashSet::new();
    let mut type_entries = Vec::new();
    for coach in real_coaches {
        let code = coach.type_code.to_ascii_uppercase();
        if seen_types.insert(code.clone()) {
            type_entries.push(format!("{code} = {}", map_vehicle_type(&code)));
        }
    }
    type_entries.sort();
    legend.extend(type_entries);

    let mut seen_services = HashSet::new();
    let mut service_entries = Vec::new();
    for coach in real_coaches {
        for code in &coach.services {
            let normalized = code.to_ascii_uppercase();
            if seen_services.insert(normalized.clone()) {
                service_entries.push(format!("{normalized} = {}", service_description(&normalized)));
            }
        }
    }
    service_entries.sort();
    legend.extend(service_entries);

    legend
}

fn build_parsed_summary(
    real_coaches: &[FormationCoach],
    sector_markers: &[char],
    front_padding: usize,
    rear_padding: usize,
) -> WagonFormationParsed {
    let mut sectors_seen = Vec::new();
    let mut seen = HashSet::new();
    for marker in sector_markers {
        let marker_str = marker.to_string();
        if seen.insert(marker_str.clone()) {
            sectors_seen.push(marker_str);
        }
    }
    for coach in real_coaches {
        if let Some(sector) = coach.sector {
            let sector_str = sector.to_string();
            if seen.insert(sector_str.clone()) {
                sectors_seen.push(sector_str);
            }
        }
    }

    WagonFormationParsed {
        coaches: real_coaches
            .iter()
            .map(|coach| WagonFormationCoach {
                id: coach_label(coach),
                vehicle_type: coach.type_code.to_ascii_uppercase(),
                class_label: map_vehicle_type(&coach.type_code).to_string(),
                sector: coach.sector.map(|value| value.to_string()),
                services: coach
                    .services
                    .iter()
                    .map(|value| value.to_ascii_uppercase())
                    .collect(),
                no_passage_prev: coach.no_pass_left,
                no_passage_next: coach.no_pass_right,
            })
            .collect(),
        padding: WagonFormationPadding {
            front: front_padding,
            rear: rear_padding,
        },
        sectors_seen,
    }
}

fn build_sector_ranges(real_coaches: &[FormationCoach]) -> Vec<WagonFormationSectorRange> {
    let mut order = Vec::new();
    let mut ranges: HashMap<String, (usize, usize)> = HashMap::new();
    for (idx, coach) in real_coaches.iter().enumerate() {
        let Some(sector) = coach.sector.map(|value| value.to_string()) else {
            continue;
        };
        if !ranges.contains_key(&sector) {
            order.push(sector.clone());
        }
        ranges
            .entry(sector)
            .and_modify(|(_, last)| *last = idx)
            .or_insert((idx, idx));
    }

    order
        .into_iter()
        .filter_map(|sector| {
            let (first_idx, last_idx) = ranges.get(&sector).copied()?;
            Some(WagonFormationSectorRange {
                sector,
                from_coach: coach_label(&real_coaches[first_idx]),
                to_coach: coach_label(&real_coaches[last_idx]),
            })
        })
        .collect()
}

fn build_accessible_coach_list(real_coaches: &[FormationCoach]) -> Vec<String> {
    real_coaches
        .iter()
        .filter(|coach| {
            coach.services.iter().any(|service| {
                matches!(
                    service.to_ascii_uppercase().as_str(),
                    "BHP" | "KW" | "NF" | "VR" | "VH"
                )
            })
        })
        .map(|coach| coach_label(coach))
        .collect()
}

fn build_class_coach_list<F>(real_coaches: &[FormationCoach], predicate: F) -> Vec<String>
where
    F: Fn(&FormationCoach) -> bool,
{
    real_coaches
        .iter()
        .filter(|coach| predicate(coach))
        .map(|coach| coach_label(coach))
        .collect()
}

fn render_human_display(
    parsed: &WagonFormationParsed,
    sector_to_coach_range: &[WagonFormationSectorRange],
    accessible_coaches: &[String],
    first_class_coaches: &[String],
    second_class_coaches: &[String],
    boarding_hint: &str,
    diagram: &str,
) -> String {
    let sectors = if sector_to_coach_range.is_empty() {
        "none".to_string()
    } else {
        sector_to_coach_range
            .iter()
            .map(|entry| {
                if entry.from_coach == entry.to_coach {
                    format!("{}: {}", entry.sector, entry.from_coach)
                } else {
                    format!("{}: {}..{}", entry.sector, entry.from_coach, entry.to_coach)
                }
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };

    let _ = parsed;
    let first_class = list_or_none(first_class_coaches);
    let second_class = list_or_none(second_class_coaches);
    let accessible = if accessible_coaches.is_empty() {
        "none".to_string()
    } else {
        let mut details = Vec::new();
        for coach_id in accessible_coaches {
            let services = parsed
                .coaches
                .iter()
                .find(|coach| &coach.id == coach_id)
                .map(|coach| coach.services.join("+"))
                .unwrap_or_default();
            if services.is_empty() {
                details.push(coach_id.clone());
            } else {
                details.push(format!("{coach_id} ({services})"));
            }
        }
        details.join(", ")
    };

    format!(
        "Boarding hint: {boarding_hint}\nDirection: front -> rear\nSectors: {sectors}\n2nd class: {second_class}\n1st class: {first_class}\nAccessible: {accessible}\nDiagram: {diagram}"
    )
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

fn build_boarding_hint(parsed: &WagonFormationParsed, accessible_coaches: &[String]) -> Option<String> {
    let preferred = accessible_coaches
        .iter()
        .find_map(|coach_id| {
            parsed
                .coaches
                .iter()
                .find(|coach| &coach.id == coach_id && coach.sector.is_some())
        })
        .or_else(|| parsed.coaches.iter().find(|coach| coach.sector.is_some()))?;

    let sector = preferred.sector.as_deref()?;
    let features = preferred
        .services
        .iter()
        .map(|code| service_description(code))
        .collect::<Vec<_>>();

    if features.is_empty() {
        Some(format!("Go to sector {sector} for coach {}.", preferred.id))
    } else {
        Some(format!(
            "Go to sector {sector} for coach {} ({})",
            preferred.id,
            features.join(", ")
        ))
    }
}

fn coach_has_first_class(coach: &FormationCoach) -> bool {
    let code = coach.type_code.to_ascii_uppercase();
    matches!(code.as_str(), "1" | "W1" | "12")
}

fn coach_has_second_class(coach: &FormationCoach) -> bool {
    let code = coach.type_code.to_ascii_uppercase();
    matches!(code.as_str(), "2" | "W2" | "12")
}

fn map_vehicle_type(type_code: &str) -> &'static str {
    match type_code.to_ascii_uppercase().as_str() {
        "1" => "1st class",
        "2" => "2nd class",
        "12" => "mixed 1st/2nd class",
        "CC" => "couchette coach",
        "FA" => "family coach",
        "WL" => "sleeping coach",
        "WR" => "restaurant coach",
        "W1" => "dining + 1st class",
        "W2" => "dining + 2nd class",
        "LK" => "traction unit",
        "D" => "baggage coach",
        "F" => "fictitious padding coach",
        "K" => "classless coach",
        "X" => "stabled coach",
        _ => "coach",
    }
}

fn service_description(code: &str) -> String {
    match code.to_ascii_uppercase().as_str() {
        "BHP" => "wheelchair spaces".to_string(),
        "BZ" => "business zone".to_string(),
        "FZ" => "family zone".to_string(),
        "KW" => "stroller area".to_string(),
        "NF" => "low-floor access".to_string(),
        "VH" => "bike spaces".to_string(),
        "VR" => "bike spaces (reservation required)".to_string(),
        other => format!("special service ({other})"),
    }
}

fn extract_train_number_candidates(trip: &Trip) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let digit_regex = Regex::new(r"\d{1,6}").ok();

    for leg in &trip.legs {
        for source in [leg.line.as_deref(), Some(leg.service.as_str())] {
            if let (Some(text), Some(regex)) = (source, digit_regex.as_ref()) {
                for m in regex.find_iter(text) {
                    let value = m.as_str().trim_start_matches('0').to_string();
                    let normalized = if value.is_empty() {
                        "0".to_string()
                    } else {
                        value
                    };
                    if seen.insert(normalized.clone()) {
                        candidates.push(normalized);
                    }
                }
            }
        }
    }

    for segment in trip
        .id
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
    {
        if segment.chars().all(|ch| ch.is_ascii_digit()) && segment.len() <= 6 {
            let normalized = segment.trim_start_matches('0').to_string();
            let normalized = if normalized.is_empty() {
                "0".to_string()
            } else {
                normalized
            };
            if seen.insert(normalized.clone()) {
                candidates.push(normalized);
            }
        }
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::render_formation_short_string;

    #[test]
    fn renders_short_string_to_readable_text_and_diagram() {
        let input =
            "@A,F,F,F,F,[(2:1#VR,2:2,2:3@B,2:4,2:5,2:6,2:7@C,2:8,2:9#BHP;KW@D,W1:10,1:11,1:12,1):14],F";

        let rendered = render_formation_short_string(input).expect("render output expected");

        assert!(rendered.diagram.contains("|2:1 VR|"));
        assert!(rendered.diagram.contains("|2:9 BHP+KW @D|"));
        assert!(!rendered.diagram.contains("|F|"));
        assert!(!rendered.diagram.contains("|X|"));
        assert!(rendered.boarding_hint.contains("Go to sector D for coach 2:9"));
        assert!(rendered.human_display.contains("Boarding hint:"));
        assert!(rendered.human_display.contains("Direction: front -> rear"));
        assert!(rendered.human_display.contains("Sectors: B: 2:3"));
        assert!(rendered.human_display.contains("1st class: W1:10, 1:11, 1:12, 1:14"));
        assert!(rendered.readable.contains("wheelchair spaces"));
        assert!(rendered.readable.contains("stroller area"));
        assert!(rendered.readable.contains("Padding coaches: front 4, rear 1."));
        assert!(
            rendered
                .legend
                .iter()
                .any(|line| line.contains("VR = bike spaces (reservation required)"))
        );
        assert!(
            rendered
                .legend
                .iter()
                .any(|line| line.contains("BHP = wheelchair spaces"))
        );
        assert_eq!(rendered.parsed.padding.front, 4);
        assert_eq!(rendered.parsed.padding.rear, 1);
        assert!(
            rendered
                .parsed
                .coaches
                .iter()
                .any(|coach| coach.id == "2:9" && coach.services == vec!["BHP", "KW"])
        );
        assert!(
            rendered
                .sector_to_coach_range
                .iter()
                .any(|range| range.sector == "D" && range.from_coach == "2:9")
        );
        assert!(rendered.accessible_coaches.contains(&"2:9".to_string()));
        assert!(rendered.first_class_coaches.contains(&"1:11".to_string()));
        assert!(rendered.second_class_coaches.contains(&"2:7".to_string()));
    }

    #[test]
    fn returns_none_for_blank_short_string() {
        assert!(render_formation_short_string("   ").is_none());
    }
}

fn infer_evu(trip: &Trip) -> Option<&'static str> {
    let operator_text = trip
        .legs
        .iter()
        .filter_map(|leg| leg.operator.as_deref())
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    if operator_text.contains("bls") {
        return Some("BLSP");
    }
    if operator_text.contains("sob") {
        return Some("SOB");
    }
    if operator_text.contains("thurbo") {
        return Some("THURBO");
    }
    if operator_text.contains("rhb") {
        return Some("RhB");
    }
    if operator_text.contains("zb") {
        return Some("ZB");
    }
    if operator_text.contains("tpf") {
        return Some("TPF");
    }
    if operator_text.contains("trn") {
        return Some("TRN");
    }
    if operator_text.contains("mbc") {
        return Some("MBC");
    }
    if operator_text.contains("oebb") || operator_text.contains("öbb") {
        return Some("OeBB");
    }
    if operator_text.contains("vdbb") {
        return Some("VDBB");
    }
    if operator_text.contains("sbb")
        || operator_text.contains("cff")
        || operator_text.contains("ffs")
    {
        return Some("SBBP");
    }

    None
}

fn find_first_formation_short_string(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                let normalized = key.to_ascii_lowercase();
                if normalized.contains("formationshortstring")
                    || normalized.contains("formation_short_string")
                {
                    if let Some(text) = extract_non_empty_text(entry) {
                        return Some(text);
                    }
                }

                if let Some(found) = find_first_formation_short_string(entry) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(find_first_formation_short_string),
        _ => None,
    }
}

fn normalize_station_name(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .replace('.', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_non_empty_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Object(map) => {
            for key in ["text", "value", "de", "fr", "it", "en"] {
                if let Some(found) = map.get(key).and_then(extract_non_empty_text) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(extract_non_empty_text),
        _ => None,
    }
}
