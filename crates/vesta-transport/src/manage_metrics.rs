//! Passive metrics projections, never an authority to query the game console.
use super::*;
use std::collections::BTreeMap;
use vesta_control::metrics::{Resolution, SERIES};
pub(super) fn implemented(path: &str) -> bool {
    matches!(
        path,
        "/api/v3/manage/metrics/current"
            | "/api/v3/manage/metrics"
            | "/api/v3/manage/players/activity"
    )
}
fn fields(query: Option<&str>, allowed: &[&str]) -> Option<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    if let Some(query) = query {
        if query.is_empty() || query.len() > 512 {
            return None;
        }
        for field in query.split('&') {
            let (name, value) = field.split_once('=')?;
            if !allowed.contains(&name)
                || value.is_empty()
                || result.insert(name.to_owned(), value.to_owned()).is_some()
            {
                return None;
            }
        }
    }
    Some(result)
}
fn number(value: &str) -> Option<i64> {
    if value.is_empty() || !value.bytes().all(|v| v.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}
fn range(query: Option<&str>) -> Option<(String, i64, i64, Resolution)> {
    let fields = fields(query, &["series", "from", "to", "resolution"])?;
    let series = fields.get("series")?.clone();
    if !SERIES.contains(&series.as_str()) {
        return None;
    }
    let from = number(fields.get("from")?)?;
    let to = number(fields.get("to")?)?;
    if to < from || to.saturating_sub(from) > 730 * 86_400_000 {
        return None;
    }
    let resolution = Resolution::parse(fields.get("resolution").map_or("auto", String::as_str))?;
    Some((series, from, to, resolution))
}
pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    if request.method != Method::GET || !request.bytes.is_empty() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let capability = if request.path.ends_with("/activity") {
        Capability::PlayersView
    } else {
        Capability::MetricsView
    };
    let Some(token) = request.authorization else {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    };
    let subject = match host.management().authorize(token, capability, None) {
        Ok(v) => v,
        Err(e) => return management_error(e),
    };
    let operations = host.runtime().operations_service();
    if request.path.ends_with("/current") {
        if request.query.is_some() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        return match operations.metrics_current(&subject) {
            Ok(v) => ok(json!(v)),
            Err(e) => operation_error(e),
        };
    }
    if request.path.ends_with("/activity") {
        let Some(values) = fields(request.query, &["after", "limit"]) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let Some(after) = number(values.get("after").map_or("0", String::as_str)) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let Some(limit) = number(values.get("limit").map_or("50", String::as_str))
            .filter(|v| (1..=100).contains(v))
        else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return match operations.player_activity(&subject, after, limit as usize + 1) {
            Ok(mut items) => {
                let more = items.len() > limit as usize;
                items.truncate(limit as usize);
                let next = if more {
                    items.last().map(|v| v.id)
                } else {
                    None
                };
                ok(
                    json!({"items":items,"next_after":next,"source":"console","completeness":"best_effort"}),
                )
            }
            Err(e) => operation_error(e),
        };
    }
    let Some((series, from, to, resolution)) = range(request.query) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    match operations.metrics_range(&subject, &series, from, to, resolution) {
        Ok(v) => ok(json!(v)),
        Err(e) => operation_error(e),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metrics_range_query_is_closed_and_bounded() {
        assert!(range(Some("series=tps&from=0&to=1")).is_some());
        for query in [
            "series=tps&series=mspt&from=0&to=1",
            "series=command&from=0&to=1",
            "series=tps&from=-1&to=1",
            "series=tps&from=2&to=1",
            "series=tps&from=0&to=1&resolution=minute",
            "series=tps&from=0&to=1&extra=private",
            "series=tps&from=0&to=9223372036854775807",
        ] {
            assert!(range(Some(query)).is_none(), "{query}");
        }
    }
}
