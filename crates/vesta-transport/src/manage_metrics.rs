//! Bounded telemetry projections and owner-configured retention. Reads never
//! issue console commands; command polling requires separately stored opt-in.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use vesta_control::metrics::{Fetch, PolicyDocument, Resolution};
pub(super) fn implemented(path: &str) -> bool {
    matches!(
        path,
        "/api/v3/manage/metrics/current"
            | "/api/v3/manage/metrics"
            | "/api/v3/manage/metrics/policy"
            | "/api/v3/manage/metrics/stream"
            | "/api/v3/manage/players/activity"
    )
}
fn fields(query: Option<&str>, allowed: &[&str]) -> Option<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    if let Some(query) = query {
        if query.is_empty() || query.len() > 1024 {
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
fn range(query: Option<&str>) -> Option<Fetch> {
    let fields = fields(
        query,
        &["series", "from", "to", "resolution", "max_points", "after"],
    )?;
    let request = Fetch {
        series: fields
            .get("series")?
            .split(',')
            .map(str::to_owned)
            .collect(),
        from: number(fields.get("from")?)?,
        to: number(fields.get("to")?)?,
        resolution: Resolution::parse(fields.get("resolution").map_or("auto", String::as_str))?,
        max_points: usize::try_from(number(
            fields.get("max_points").map_or("2000", String::as_str),
        )?)
        .ok()?,
        after: match fields.get("after") {
            Some(value) => Some(number(value)?),
            None => None,
        },
    };
    request.valid().then_some(request)
}
fn etag(request: &Request<'_>, revision: i64) -> String {
    let mut hash = Sha256::new();
    hash.update(request.path);
    hash.update([0]);
    hash.update(request.query.unwrap_or(""));
    format!("\"metrics-{revision}-{:x}\"", hash.finalize())
}
fn tagged(mut reply: Reply, tag: &str) -> Reply {
    reply.headers_mut().insert(
        "etag",
        http::HeaderValue::from_str(tag).expect("fixed digest etag"),
    );
    reply
}
fn unchanged(tag: &str) -> Reply {
    tagged(
        http::Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header("cache-control", "no-store")
            .body(crate::ReplyBody::Bytes(hyper::body::Bytes::new()))
            .expect("fixed response"),
        tag,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutPolicy {
    expected_revision: Option<i64>,
    #[serde(flatten)]
    document: PolicyDocument,
}
pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    // This endpoint supports one strong validator. Explicitly refuse wildcard,
    // lists and weak validators rather than silently mishandling RFC variants.
    if request.if_none_match.is_some_and(|v| {
        v.len() < 2
            || v.len() > 128
            || !v.starts_with('"')
            || !v.ends_with('"')
            || !v[1..v.len() - 1]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    }) {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let policy = request.path.ends_with("/policy");
    if request.path.ends_with("/stream") {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    if !(request.method == Method::GET || policy && request.method == Method::PUT)
        || (request.method == Method::GET && !request.bytes.is_empty())
        || request.range.is_some()
        || request.if_range.is_some()
        || request.if_match.is_some()
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let capability = if request.path.ends_with("/activity") {
        Capability::PlayersView
    } else if policy && request.method == Method::PUT {
        Capability::ConfigEdit
    } else if policy {
        Capability::ConfigView
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
    if policy {
        if request.query.is_some() || request.if_none_match.is_some() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        if request.method == Method::PUT {
            let Some(value) = decode::<PutPolicy>(request.bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            return match operations.set_metrics_policy(
                &subject,
                value.expected_revision,
                value.document,
            ) {
                Ok(revision) => ok(json!({"source":"stored","revision":revision})),
                Err(OperationError::Rejected {
                    code: OperationErrorCode::SourceRevisionUnavailable,
                    ..
                }) => error(StatusCode::CONFLICT, "metrics_policy_conflict"),
                Err(e) => operation_error(e),
            };
        }
        return match operations.metrics_policy(&subject, host.configuration().metrics()) {
            Ok(v) => ok(json!(v)),
            Err(e) => operation_error(e),
        };
    }
    if request.path.ends_with("/current") {
        if request.query.is_some() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        return match operations.metrics_current(&subject) {
            Ok(v) => {
                let tag = etag(&request, v.status.revision);
                if request.if_none_match == Some(tag.as_str()) {
                    unchanged(&tag)
                } else {
                    tagged(ok(json!(v)), &tag)
                }
            }
            Err(e) => operation_error(e),
        };
    }
    if request.path.ends_with("/activity") {
        if request.if_none_match.is_some() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
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
    let Some(fetch) = range(request.query) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    // Conditional polling reads just the current status, not historical buckets.
    // This is freshly authorized, including for an eventual 304 response.
    if request.if_none_match.is_some() {
        let current = match operations.metrics_current(&subject) {
            Ok(v) => v,
            Err(e) => return operation_error(e),
        };
        if fetch
            .after
            .is_some_and(|after| after > current.status.revision)
        {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let tag = etag(&request, current.status.revision);
        if request.if_none_match == Some(tag.as_str()) {
            return unchanged(&tag);
        }
    }
    match operations.metrics_fetch(&subject, &fetch, host.configuration().metrics()) {
        Ok(v) => {
            let tag = etag(&request, v.cursor);
            tagged(ok(json!(v)), &tag)
        }
        Err(e) => operation_error(e),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metrics_range_query_is_closed_and_bounded() {
        assert!(range(Some("series=tps,mspt&from=0&to=1&max_points=10&after=0")).is_some());
        for query in [
            "series=tps&series=mspt&from=0&to=1",
            "series=command&from=0&to=1",
            "series=tps&from=-1&to=1",
            "series=tps&from=2&to=1",
            "series=tps&from=0&to=1&resolution=minute",
            "series=tps&from=0&to=1&extra=private",
            "series=tps&from=0&to=9223372036854775807",
            "series=tps,tps&from=0&to=1",
            "series=tps&from=0&to=1&max_points=0",
            "series=tps&from=0&to=1&max_points=2001",
            "series=tps&from=0&to=1&after=-1",
        ] {
            assert!(range(Some(query)).is_none(), "{query}");
        }
    }
    #[test]
    fn metrics_policy_put_is_closed_and_consent_defaults_off() {
        let document = serde_json::json!({"expected_revision":null,"settings":vesta_control::config::MetricsSettings::default()});
        let parsed = decode::<PutPolicy>(&serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(!parsed.document.command_polling.enabled);
        assert!(parsed.document.valid());
        let mut invalid = document;
        invalid["unexpected"] = json!(true);
        assert!(decode::<PutPolicy>(&serde_json::to_vec(&invalid).unwrap()).is_none());
    }
}
