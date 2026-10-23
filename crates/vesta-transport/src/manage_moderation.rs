//! Typed player authority is separate from raw console authority.
use super::*;
use vesta_control::moderation::{self, ModerationAction};

macro_rules! request {
    ($($variant:ident $( { $($field:ident : $ty:ty),+ } )?),+ $(,)?) => {
        #[derive(Deserialize)]
        #[serde(tag="action", rename_all="snake_case", deny_unknown_fields)]
        enum Moderate {
            $($variant { player: String, expected_generation: String, confirmation: Option<String>, $($($field: $ty),+)? }),+
        }
        impl Moderate {
            fn parts(self) -> (ModerationAction, String, Option<String>) {
                match self {
                    $(Self::$variant { player, expected_generation, confirmation, $($($field),+)? } =>
                        (ModerationAction::$variant { player, $($($field),+)? }, expected_generation, confirmation)),+
                }
            }
        }
    }
}
request!(Kick { reason: Option<String> }, Ban { reason: Option<String>, duration_seconds: Option<u64> }, Pardon, WhitelistAdd, WhitelistRemove, Op, Deop, Kill, Lightning);

pub(super) fn implemented(path: &str) -> bool {
    matches!(
        path,
        "/api/v3/manage/players/moderate"
            | "/api/v3/manage/players/moderation"
            | "/api/v3/manage/players/bans"
            | "/api/v3/manage/players/ops"
            | "/api/v3/manage/players/whitelist"
            | "/api/v3/manage/players/whitelist/offline"
            | "/api/v3/manage/players/whitelist/startup"
    )
}
pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    let Request {
        method,
        path,
        query,
        authorization,
        idempotency,
        bytes,
        ..
    } = request;
    if path.ends_with("/whitelist/startup") {
        if (method == Method::POST && query.is_some())
            || (method != Method::POST && method != Method::GET)
        {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let Some(token) = authorization else {
            return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
        };
        let capability = if method == Method::GET {
            Capability::PlayersView
        } else {
            Capability::PlayersModerate
        };
        let subject = match host.management().authorize(token, capability, None) {
            Ok(s) => s,
            Err(e) => return management_error(e),
        };
        if method == Method::GET {
            let result = if let Some(query) = query {
                let Some(selection) = query.strip_prefix("ids=").filter(|s| s.len() <= 1200) else {
                    return error(StatusCode::BAD_REQUEST, "invalid_request");
                };
                host.files().startup_whitelist_receipts_selected(
                    &subject,
                    &selection
                        .split(',')
                        .take(33)
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                )
            } else {
                host.files().startup_whitelist_receipts(&subject)
            };
            return match result {
                Ok(v) => ok(v),
                Err(e) => super::files::file_error(e),
            };
        }
        let Some(patch) = decode::<vesta_control::files::StartupWhitelistPatch>(bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let Some(key) = key(
            idempotency,
            host.management().server_id(),
            subject.device.as_str(),
            path,
        ) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return match host
            .files()
            .startup_whitelist_update(host.runtime(), &subject, &key, patch)
        {
            Ok(v) => ok(v),
            Err(e) => super::files::file_error(e),
        };
    }
    if path.ends_with("/whitelist/offline") {
        if query.is_some() || (method != Method::GET && method != Method::POST) {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let Some(token) = authorization else {
            return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
        };
        let cap = if method == Method::POST {
            Capability::PlayersModerate
        } else {
            Capability::PlayersView
        };
        let subject = match host.management().authorize(token, cap, None) {
            Ok(s) => s,
            Err(e) => return management_error(e),
        };
        let result = if method == Method::GET {
            host.files().offline_whitelist(&subject)
        } else {
            let Some(patch) = decode::<vesta_control::files::OfflineWhitelistPatch>(bytes) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            let Some(key) = key(
                idempotency,
                host.management().server_id(),
                subject.device.as_str(),
                path,
            ) else {
                return error(StatusCode::BAD_REQUEST, "invalid_request");
            };
            host.files()
                .patch_offline_whitelist(host.runtime(), &subject, &key, patch)
        };
        return match result {
            Ok(value) => ok(value),
            Err(vesta_control::files::FileError::Corrupt) => {
                error(StatusCode::SERVICE_UNAVAILABLE, "players_file_malformed")
            }
            Err(e) => super::files::file_error(e),
        };
    }
    let mutation = path.ends_with("/moderate");
    if (mutation && method != Method::POST) || (!mutation && method != Method::GET) {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let input = if mutation {
        if query.is_some() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let Some(input) = decode::<Moderate>(bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let (action, generation, confirmation) = input.parts();
        let Ok(action) = action.validated() else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        Some((action, generation, confirmation))
    } else {
        None
    };
    let capability = input
        .as_ref()
        .map_or(Capability::PlayersView, |v| v.0.capability());
    let Some(token) = authorization else {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    };
    let subject = match host.management().authorize(token, capability, None) {
        Ok(subject) => subject,
        Err(e) => return management_error(e),
    };
    if let Some((action, generation, confirmation)) = input {
        let Some(key) = key(
            idempotency,
            host.management().server_id(),
            subject.device.as_str(),
            path,
        ) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let Ok(generation) = GenerationId::parse_hex(&generation) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        if confirmation.is_some() && !matches!(action, ModerationAction::Op { .. }) {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        if matches!(action, ModerationAction::Lightning { .. }) {
            let mut version = host.configuration().pack().minecraft_version().split('.');
            if version.next() == Some("1")
                && version
                    .next()
                    .and_then(|v| v.parse::<u32>().ok())
                    .is_some_and(|v| v < 13)
            {
                return error(
                    StatusCode::PRECONDITION_FAILED,
                    "lightning_unsupported_version",
                );
            }
        }
        let runtime = host.runtime();
        let mut value = match runtime.submit_game(
            &subject,
            GameCommand::Moderate {
                expected_generation: generation,
                action,
            },
            &key,
        ) {
            Ok(Admission::Created(v) | Admission::Existing(v)) => v,
            Err(e) => return operation_error(e),
        };
        if value.state == OperationState::AwaitingConfirmation {
            if let Some(token) = confirmation {
                let token = match ConfirmationToken::try_from(token.into_bytes()) {
                    Ok(v) => v,
                    Err(e) => return operation_error(e),
                };
                value = match runtime.confirm(&subject, &value.id, token) {
                    Ok(v) => v,
                    Err(e) => return operation_error(e),
                };
            } else {
                let receipt =
                    match runtime.issue_confirmation(&subject, &value.id, Duration::from_secs(120))
                    {
                        Ok(v) => v,
                        Err(e) => return operation_error(e),
                    };
                return envelope(
                    StatusCode::ACCEPTED,
                    json!({"data":{"operation_id":value.id.as_str(),"state":"awaiting_confirmation","confirmation_token":receipt.token.expose_secret(),"expires_at_ms":receipt.expires_at_ms}}),
                );
            }
        }
        return envelope(StatusCode::ACCEPTED, json!({"data":summary(&value)}));
    }
    if path.ends_with("/moderation") {
        let Some((after, limit)) = page(query, "moderation", host.management().server_id(), 100)
        else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        let Ok(after) = i64::try_from(after) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return match host.runtime().operations_service().moderation_history(
            &subject,
            after,
            limit + 1,
        ) {
            Ok(mut items) => {
                let more = items.len() > limit;
                items.truncate(limit);
                let next = if more {
                    items
                        .last()
                        .map(|v| cursor(host.management().server_id(), "moderation", v.id))
                } else {
                    None
                };
                ok(json!({"items":items,"next_cursor":next}))
            }
            Err(e) => operation_error(e),
        };
    }
    if query.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let directory = host.configuration().game();
    let items = if path.ends_with("/bans") {
        moderation::bans(directory)
            .and_then(|v| serde_json::to_value(v).map_err(|_| "players_file_malformed"))
    } else if path.ends_with("/ops") {
        moderation::ops(directory)
            .and_then(|v| serde_json::to_value(v).map_err(|_| "players_file_malformed"))
    } else {
        host.files()
            .projected_whitelist(host.runtime())
            .map_err(|_| "players_file_malformed")
            .and_then(|v| serde_json::to_value(v).map_err(|_| "players_file_malformed"))
    };
    match items {
        Ok(items) => ok(json!({"items":items})),
        Err(code) => error(StatusCode::SERVICE_UNAVAILABLE, code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_route_request_is_closed_and_action_specific() {
        for raw in [
            r#"{"action":"op","player":"Alex","expected_generation":"x","reason":"no"}"#,
            r#"{"action":"kick","player":"Alex","expected_generation":"x","duration_seconds":60}"#,
            r#"{"action":"ban","player":"Alex","expected_generation":"x","line":"stop"}"#,
            r#"{"action":"execute_as_player","player":"Alex","expected_generation":"x"}"#,
            r#"{"action":"op","player":"Alex","player":"Steve","expected_generation":"x"}"#,
        ] {
            assert!(decode::<Moderate>(raw.as_bytes()).is_none());
        }
        let input = decode::<Moderate>(br#"{"action":"ban","player":"Alex","reason":"why","duration_seconds":60,"expected_generation":"x"}"#).unwrap();
        assert_eq!(input.parts().0.duration_seconds(), Some(60));
        assert!(decode::<Moderate>(b"[]").is_none());
    }
}
