//! Content management is capability-gated independently of owner software control.
use super::{Request, decode, error, management_error, ok};
use crate::Reply;
use http::{Method, StatusCode};
use serde::Deserialize;
use vesta_control::{content::*, host::NetworkHost, identity::Capability};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Install {
    #[serde(default)]
    kind: ContentKind,
    provider: String,
    project_id: String,
    version_id: String,
    path: Option<String>,
    expected_sha256: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cancel {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identify {
    path: String,
}
fn value(result: vesta_control::files::Result<impl serde::Serialize>) -> Reply {
    match result {
        Ok(result) => match serde_json::to_value(result) {
            Ok(result) => ok(result),
            Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
        },
        Err(error) => super::files::file_error(error),
    }
}
pub(super) fn dispatch(host: &NetworkHost, request: Request<'_>) -> Reply {
    let Some(token) = request.authorization else {
        return error(StatusCode::UNAUTHORIZED, "management_session_invalid");
    };
    let subject = match host.management().authenticate(token) {
        Ok(subject) => subject,
        Err(e) => return management_error(e),
    };
    if request.query.is_some()
        || request.if_match.is_some()
        || request.if_none_match.is_some()
        || request.range.is_some()
        || request.if_range.is_some()
        || request.bytes.len() > 65536
    {
        return error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let action = request
        .path
        .strip_prefix("/api/v3/manage/")
        .unwrap_or_default();
    if action == "content" && request.method == Method::GET {
        if !request.bytes.is_empty() {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let (family, version) = match host.software().map(|s| s.content_target()).transpose() {
            Ok(value) => value.unwrap_or((None, None)),
            Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "software_state_unavailable",
                );
            }
        };
        let world = match host.files().world(&subject) {
            Ok(world) => world,
            Err(e) => return super::files::file_error(e),
        };
        return value(host.files().content_inventory(
            &subject,
            family,
            version.unwrap_or_else(|| host.configuration().pack().minecraft_version().into()),
            &world,
        ));
    }
    if request.method != Method::POST {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    if let Err(e) = host
        .management()
        .authorize(token, Capability::ContentInstall, None)
    {
        return management_error(e);
    }
    let Some(key) = super::key(
        request.idempotency,
        host.database().server_id(),
        subject.device.as_str(),
        request.path,
    ) else {
        return error(StatusCode::BAD_REQUEST, "idempotency_key_required");
    };
    if action == "content/mutate" {
        let Some(input) = decode::<ContentMutation>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        if matches!(
            &input,
            ContentMutation::Enable {
                kind: ContentKind::Datapack,
                ..
            } | ContentMutation::Disable {
                kind: ContentKind::Datapack,
                ..
            }
        ) {
            return error(
                StatusCode::CONFLICT,
                "content_datapack_toggle_requires_world",
            );
        }
        return value(
            host.files()
                .content_mutate(host.runtime(), &subject, &key, input),
        );
    }
    if action == "content/cancel" {
        let Some(input) = decode::<Cancel>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return value(
            host.files()
                .content_cancel_remote(&subject, &input.id, &key),
        );
    }
    if action == "content/identify" {
        let Some(input) = decode::<Identify>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        return value(host.files().content_identify(&subject, &input.path));
    }
    if matches!(action, "content/install" | "content/update") {
        let Some(input) = decode::<Install>(request.bytes) else {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        };
        if (action == "content/update") != (input.path.is_some() && input.expected_sha256.is_some())
            || (input.path.is_some() != input.expected_sha256.is_some())
        {
            return error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        let provider = match input.provider.as_str() {
            "modrinth" => Provider::Modrinth,
            "curseforge" => Provider::Curseforge,
            _ => return error(StatusCode::BAD_REQUEST, "invalid_request"),
        };
        let (family, _version) = match host.software().map(|s| s.content_target()).transpose() {
            Ok(value) => value.unwrap_or((None, None)),
            Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "software_state_unavailable",
                );
            }
        };
        let world = match host.files().world(&subject) {
            Ok(world) => world,
            Err(e) => return super::files::file_error(e),
        };
        let directory = match input.kind.directory(family.as_deref(), &world.name) {
            Ok(dir) => dir,
            Err(ContentError::ModsUnsupported) => {
                return error(StatusCode::CONFLICT, "content_mods_unsupported");
            }
            Err(ContentError::PluginsUnsupported) => {
                return error(StatusCode::CONFLICT, "content_plugins_unsupported");
            }
            Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_request"),
        };
        let credential = std::env::var("VESTA_CURSEFORGE_API_KEY").ok().filter(|s| {
            !s.is_empty() && s.len() <= 1024 && s.bytes().all(|b| b.is_ascii_graphic())
        });
        let providers = UreqProviderClient::new(credential);
        let http = UreqDownloadHttp::default();
        let registry = TrustedSourceRegistry::default();
        let installer = ContentInstaller::new(host.files(), &providers, &http, &registry);
        let replace = input.path.as_deref().zip(input.expected_sha256.as_deref());
        return match installer.install_content(
            host.runtime(),
            &subject,
            &key,
            &InstallRequest::Provider {
                provider,
                project_id: input.project_id,
                version_id: input.version_id,
            },
            input.kind,
            &directory,
            replace,
        ) {
            Ok(change) => value(Ok(change)),
            Err(ContentError::File(e)) => super::files::file_error(e),
            Err(
                ContentError::InvalidRequest
                | ContentError::UnsafeFilename
                | ContentError::InvalidMetadata,
            ) => error(StatusCode::BAD_REQUEST, "invalid_request"),
            Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
        };
    }
    error(StatusCode::BAD_REQUEST, "invalid_request")
}
