//! Listener-free adapter for existing per-server management handlers.
use http::{Method, Response};
use vesta_control::worker::{ServerSubject, ServerWorker};

/// Only server-relative management suffixes and allowlisted metadata cross this
/// boundary. Host authentication, cookies and forwarding headers are absent.
pub struct WorkerRequest<'a> {
    pub method: &'a Method,
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub body: &'a [u8],
    pub idempotency_key: Option<&'a str>,
    pub if_match: Option<&'a str>,
    pub if_none_match: Option<&'a str>,
    pub range: Option<&'a str>,
    pub if_range: Option<&'a str>,
}
/// Caller must already have authenticated the subject at the host boundary.
/// This is never mounted directly as a public subject-taking HTTP endpoint.
pub fn dispatch(
    worker: &dyn ServerWorker,
    subject: ServerSubject,
    request: WorkerRequest<'_>,
) -> Response<impl hyper::body::Body<Data = hyper::body::Bytes, Error = std::io::Error> + Send> {
    dispatch_inner(worker, subject, request)
}
fn dispatch_inner(
    worker: &dyn ServerWorker,
    subject: ServerSubject,
    request: WorkerRequest<'_>,
) -> crate::Reply {
    use http::StatusCode;
    let host = match worker.authorize(subject) {
        Ok(host) => host,
        Err(e) => return crate::manage::management_error(e),
    };
    let suffix = request.path.trim_start_matches('/');
    if suffix.is_empty()
        || suffix.contains('?')
        || suffix.contains('#')
        || suffix.split('/').any(|p| p == "." || p == "..")
        || suffix.contains('%')
    {
        return crate::manage::error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    // Identity and SFTP belong to the host. Never expose the legacy authority
    // management endpoints or allow local per-server enrollment through a worker.
    if [
        "roles",
        "users",
        "devices",
        "invitations",
        "access-sources",
        "sftp",
    ]
    .iter()
    .any(|p| suffix == *p || suffix.starts_with(&format!("{p}/")))
    {
        return crate::manage::error(StatusCode::FORBIDDEN, "management_forbidden");
    }
    let path = format!("/api/v3/manage/{suffix}");
    if !crate::manage::implemented(&path) {
        return crate::manage::error(StatusCode::NOT_FOUND, "not_found");
    }
    if request.body.len() > 4 * 1024 * 1024 {
        return crate::manage::error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
    }
    crate::manage::dispatch(
        &host,
        crate::manage::Request {
            tls_fingerprint: None,
            method: request.method,
            path: &path,
            query: request.query,
            // A private non-credential marker; the request-scoped facade authenticates
            // the supplied trusted subject and rechecks durable worker generation.
            authorization: Some("trusted-worker"),
            idempotency: request.idempotency_key,
            bytes: request.body,
            if_match: request.if_match,
            if_none_match: request.if_none_match,
            range: request.range,
            if_range: request.if_range,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::Path, sync::Arc, time::Duration};
    use uuid::Uuid;
    use vesta_control::{
        config::HostConfiguration,
        identity::{BuiltinRole, Capability},
        worker::*,
    };
    use vesta_state::{Clock, SystemClock};

    struct Fixture {
        worker: Option<Worker>,
        handle: WorkerHandle,
        subject: ServerSubject,
        policy: WorkerPolicy,
        _temp: tempfile::TempDir,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let base = temp.path();
            for dir in ["game/config", "private", "bin"] {
                std::fs::create_dir_all(base.join(dir)).unwrap();
            }
            std::fs::write(base.join("bin/inert"), b"").unwrap();
            std::fs::write(base.join("private/launch.toml"),"[minecraft]\nworking-directory = 'game'\n[launch]\nkind = 'native'\nexecutable = '../bin/inert'\nstop = 'console-stop'\n").unwrap();
            std::fs::write(base.join("game/config/test.txt"), b"worker content").unwrap();
            let document = "schema-version = 1\n[paths]\nstate = 'private-state'\ngame = 'game'\nscan-roots = ['game/config']\nlaunch-definition = 'private/launch.toml'\n[pack]\nname = 'Worker'\nversion = '1.0.0'\nrevision = 1\nminimum-revision = 1\nminecraft-version = '1.21.1'\nunresolved-disposition = 'embed-public'\n[pack-access]\nmode = 'minecraft'\n[listener]\nmode = 'disabled'\n[backups]\nrepository = 'backups'\n[backups.limits]\nmin-free-space = '0'\n[[backups.profile]]\nname = 'manual'\nmode = 'stopped'\nkeep-last = 3\n";
            let config =
                HostConfiguration::parse_toml(document.as_bytes(), base, Path::new("host.toml"))
                    .unwrap();
            std::fs::write(base.join("host.toml"), document).unwrap();
            let host = Uuid::new_v4();
            let server = Uuid::new_v4();
            let worker = Worker::open(config, Arc::new(SystemClock), None, host, server).unwrap();
            let handle = worker.handle();
            let now = SystemClock.millis();
            let subject = ServerSubject {
                host_id: host,
                server_id: server,
                account_id: Uuid::new_v4(),
                device_id: Uuid::new_v4(),
                generation: 1,
                issued_at_ms: now,
                expires_at_ms: now + 30_000,
                capabilities: BuiltinRole::Administrator
                    .capabilities()
                    .iter()
                    .map(|c| c.id().into())
                    .collect(),
            };
            let policy = WorkerPolicy {
                host_id: host,
                server_id: server,
                generation: 1,
                grants: vec![WorkerGrant {
                    account_id: subject.account_id,
                    device_id: subject.device_id,
                    role: ServerRole::Admin,
                    capabilities: subject.capabilities.clone(),
                }],
            };
            assert!(handle.authorize(subject.clone()).is_err());
            handle.sync_policy(policy.clone()).unwrap();
            Self {
                worker: Some(worker),
                handle,
                subject,
                policy,
                _temp: temp,
            }
        }
        fn request(&self, subject: ServerSubject, path: &str, query: Option<&str>) -> crate::Reply {
            dispatch_inner(
                &self.handle,
                subject,
                WorkerRequest {
                    method: &Method::GET,
                    path,
                    query,
                    body: b"",
                    idempotency_key: None,
                    if_match: None,
                    if_none_match: None,
                    range: None,
                    if_range: None,
                },
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            assert!(matches!(
                self.worker
                    .take()
                    .unwrap()
                    .shutdown(Duration::from_secs(5))
                    .unwrap(),
                WorkerShutdown::Complete
            ));
        }
    }
    #[test]
    fn trusted_worker_drives_console_files_players_pack_and_backups_without_bearer() {
        let f = Fixture::new();
        for (path, query) in [
            ("console", None),
            ("files", Some("path=config")),
            ("files/content", Some("path=config/test.txt")),
            ("players", None),
            ("pack", None),
            ("backups", None),
        ] {
            let reply = f.request(f.subject.clone(), path, query);
            if reply.status() != http::StatusCode::OK {
                if let crate::ReplyBody::Bytes(b) = reply.body() {
                    panic!("{path}: {} {}", reply.status(), String::from_utf8_lossy(b));
                }
                panic!("{path}: {}", reply.status());
            }
        }
        // No local identity or device credentials were minted by composition.
        let host = f.handle.authorize(f.subject.clone()).unwrap();
        assert_eq!(
            host.database()
                .read(
                    |c| Ok(c.query_row("SELECT count(*) FROM principals", [], |r| r
                        .get::<_, i64>(0))?)
                )
                .unwrap(),
            0
        );
        drop(host);
        for (path, body) in [
            ("files/mkdir", br#"{"path":"created-by-worker"}"#.as_slice()),
            ("backups/run", br#"{"profile":"manual"}"#.as_slice()),
        ] {
            let reply = dispatch_inner(
                &f.handle,
                f.subject.clone(),
                WorkerRequest {
                    method: &Method::POST,
                    path,
                    query: None,
                    body,
                    idempotency_key: Some("worker-test-idempotency-0001"),
                    if_match: None,
                    if_none_match: None,
                    range: None,
                    if_range: None,
                },
            );
            assert!(reply.status().is_success(), "{path}: {}", reply.status());
        }
        assert!(f._temp.path().join("game/created-by-worker").is_dir());
    }
    #[test]
    fn trusted_worker_refuses_cross_server_stale_future_expired_and_missing_capability() {
        let f = Fixture::new();
        let mut wrong = f.subject.clone();
        wrong.server_id = Uuid::new_v4();
        assert_eq!(f.request(wrong, "console", None).status(), 401);
        for generation in [0, 2] {
            let mut s = f.subject.clone();
            s.generation = generation;
            assert_eq!(f.request(s, "console", None).status(), 401);
        }
        let mut expired = f.subject.clone();
        expired.expires_at_ms = expired.issued_at_ms;
        assert_eq!(f.request(expired, "console", None).status(), 401);
        let mut limited = f.subject.clone();
        limited
            .capabilities
            .retain(|c| c != Capability::ConsoleRead.id());
        assert_eq!(f.request(limited, "console", None).status(), 403);
        let mut policy = f.policy.clone();
        policy.generation = 2;
        f.handle.sync_policy(policy).unwrap();
        assert_eq!(f.request(f.subject.clone(), "console", None).status(), 401);
        assert!(f.handle.sync_policy(f.policy.clone()).is_err());
    }
    #[test]
    fn trusted_worker_rechecks_policy_on_retained_facade_and_refuses_legacy_identity_routes() {
        let f = Fixture::new();
        let facade = f.handle.authorize(f.subject.clone()).unwrap();
        assert!(
            facade
                .management()
                .authorize("no bearer", Capability::ConsoleRead, None)
                .is_ok()
        );
        assert_eq!(f.request(f.subject.clone(), "roles", None).status(), 403);
        let mut policy = f.policy.clone();
        policy.generation = 2;
        policy.grants.clear();
        f.handle.sync_policy(policy).unwrap();
        assert!(
            facade
                .management()
                .authorize("no bearer", Capability::ConsoleRead, None)
                .is_err()
        );
    }

    #[test]
    fn trusted_worker_restart_requires_sync_and_preserves_generation_floor() {
        let mut f = Fixture::new();
        let config = f
            .handle
            .authorize(f.subject.clone())
            .unwrap()
            .configuration()
            .clone();
        assert!(
            Worker::from_registry(
                None,
                config.clone(),
                Arc::new(SystemClock),
                None,
                f.subject.host_id
            )
            .unwrap()
            .is_none()
        );
        let mut policy = f.policy.clone();
        policy.generation = 2;
        f.handle.sync_policy(policy.clone()).unwrap();
        assert!(matches!(
            f.worker
                .take()
                .unwrap()
                .shutdown(Duration::from_secs(5))
                .unwrap(),
            WorkerShutdown::Complete
        ));
        let entry = vesta_control::machine::registry::ServerEntry {
            server_id: f.subject.server_id,
            name: "Worker".into(),
            game_port: 25565,
            state: vesta_control::machine::registry::ServerState::Stopped,
            visibility: vesta_control::machine::registry::Visibility::Private,
        };
        let worker = Worker::from_registry(
            Some(&entry),
            config,
            Arc::new(SystemClock),
            None,
            f.subject.host_id,
        )
        .unwrap()
        .unwrap();
        f.handle = worker.handle();
        f.worker = Some(worker);
        f.subject.generation = 2;
        assert!(f.handle.authorize(f.subject.clone()).is_err());
        assert!(f.handle.sync_policy(f.policy.clone()).is_err());
        f.handle.sync_policy(policy).unwrap();
        assert_eq!(f.request(f.subject.clone(), "console", None).status(), 200);
    }

    #[test]
    fn trusted_worker_host_owner_expansion_preserves_backup_restore_without_host_identity_authority()
     {
        let mut f = Fixture::new();
        f.subject.generation = 2;
        f.subject.capabilities = BuiltinRole::Owner
            .capabilities()
            .iter()
            .map(|c| c.id().into())
            .collect();
        let mut policy = f.policy.clone();
        policy.generation = 2;
        policy.grants[0].role = ServerRole::Owner;
        policy.grants[0].capabilities = f.subject.capabilities.clone();
        f.handle.sync_policy(policy).unwrap();
        let facade = f.handle.authorize(f.subject.clone()).unwrap();
        assert!(
            facade
                .management()
                .authorize("trusted", Capability::BackupsRestore, None)
                .is_ok()
        );
        assert!(
            facade
                .management()
                .authorize("trusted", Capability::SecurityManage, None)
                .is_err()
        );
    }
}

/// Host-only projection; the registry/session checks precede this call.
pub(crate) fn dispatch_host(
    worker: &dyn ServerWorker,
    subject: ServerSubject,
    mut request: WorkerRequest<'_>,
    tls: std::sync::Arc<crate::tls::Material>,
    peer: std::net::IpAddr,
    downloads: std::sync::Arc<crate::Downloads<vesta_control::pack_access::RealtimeSession>>,
) -> crate::Reply {
    let server=subject.server_id;
    let host=match worker.authorize(subject.clone()){Ok(h)=>h,Err(e)=>return crate::manage::management_error(e)};
    let path=request.path;
    let mut reply=if let Some(suffix)=path.strip_prefix("manage/") {
        if matches!(suffix,"pack/sync"|"pack/artifact") {
            crate::pack_sync::dispatch(&host,request.method,&format!("/api/v3/manage/{suffix}"),request.query,Some("trusted-worker"),crate::routes::Requester {downloads:downloads.clone(),peer},&tls)
        }else {request.path=suffix;dispatch_inner(worker,subject,request)}
    } else if (path.starts_with("pack/") || path=="status") && *request.method==Method::GET {
        let mapped=if path=="status"{"/api/v3/status".to_owned()}else{format!("/api/v2/{path}")};
        crate::routes::dispatch(&host,&mapped,request.query,Some("trusted-worker"),request.body,crate::routes::Requester{downloads:downloads.clone(),peer},tls.clone())
    } else if path=="discovery" && *request.method==Method::POST {
        match vesta_protocol::discovery::decode_request(request.body).and_then(|r|host.discover(&r)).and_then(|m|vesta_protocol::discovery::encode_manifest(&m)) {
            Ok(bytes)=>crate::response(http::StatusCode::OK,bytes,"application/json"),Err(_)=>crate::manage::error(http::StatusCode::BAD_REQUEST,"invalid_request")
        }
    } else if path=="status" && *request.method==Method::GET {
        let mut r=request;r.path="server/status";dispatch_inner(worker,subject,r)
    } else {crate::manage::error(http::StatusCode::NOT_FOUND,"not_found")};
    // Rewrite only JSON metadata, never artifact/file bodies. Preserve validators.
    if path!="manage/files/content" && reply.headers().get("content-type").is_some_and(|v|v=="application/json") {
    if let crate::ReplyBody::Bytes(bytes)=reply.body_mut() {
        if let Ok(mut value)=serde_json::from_slice::<serde_json::Value>(bytes) {
            fn rewrite(v:&mut serde_json::Value,server:uuid::Uuid) {
                match v {
                    serde_json::Value::Object(map)=>for (key,value) in map {if key=="server_id" {*value=serde_json::json!(server);}else{rewrite(value,server)}},
                    serde_json::Value::Array(a)=>for v in a {rewrite(v,server)},
                    serde_json::Value::String(s) if !s.contains("/v1/servers/")=>{
                        for (old,new) in [("/api/v3/manage/","manage/"),("/api/v2/pack/","pack/"),("/resource-packs/","resource-packs/")] {
                            if let Some(at)=s.find(old) {s.replace_range(at..at+old.len(),&format!("/v1/servers/{server}/{new}"));break;}
                        }
                    },_=>{}
                }
            }
            rewrite(&mut value,server);
            *bytes=serde_json::to_vec(&value).unwrap().into();
            let len=bytes.len();
            reply.headers_mut().insert("content-length",len.into());
        }
    }
    }
    reply
}
