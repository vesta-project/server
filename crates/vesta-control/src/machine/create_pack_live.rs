//! Live pack content install for a modpack create. The new server is not
//! registered yet: its own worker is opened briefly from the trusted
//! `host.toml`, the owner's policy is synchronized to it, the base software the
//! create just installed is recorded as the server's current software (so the
//! pack install finds its loader in place and installs no software again), and
//! the server's own pack engine runs exactly as for an existing server (install
//! plan, then `apply_pack`, which verifies every pack file). Cancellation is
//! cooperative at the pack engine's safe boundaries: between downloads and
//! between written files. The worker is shut down before the create job
//! registers (or, on failure or cancellation, removes) the server.
use super::{CreateProgress, PackInstallRequest, PackSourceRequest};
use crate::{
    config::HostConfiguration,
    content::UreqProviderClient,
    packs::workspace::{InstallSource, PackInstallApply, PackWorkspace, UploadBegin, UploadChunk},
    worker::{ServerWorker, Worker},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    io::Read,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub(super) fn install(
    request: &PackInstallRequest<'_>,
    progress: &dyn CreateProgress,
) -> Result<(), &'static str> {
    let dir = request.server_dir;
    let text = std::fs::read(dir.join("host.toml")).map_err(|_| "install_failed")?;
    let config = HostConfiguration::parse_toml(&text, dir, Path::new("host.toml"))
        .map_err(|_| "install_failed")?;
    let worker = Worker::open(
        config,
        Arc::new(vesta_state::SystemClock),
        None,
        request.host_id,
        request.server_id,
    )
    .map_err(|_| "install_failed")?;
    let handle = worker.handle();
    // The pack engine checks this flag at its safe boundaries; a watcher turns
    // the job's cancellation request into it while the install runs.
    let cancel = Arc::new(AtomicBool::new(false));
    let finished = AtomicBool::new(false);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !finished.load(Ordering::Acquire) {
                if progress.cancelled() {
                    cancel.store(true, Ordering::Release);
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let result = handle
            .sync_policy(request.policy.clone())
            .map_err(|_| "install_failed")
            .and_then(|()| {
                handle
                    .authorize(request.server_subject.clone())
                    .map_err(|_| "install_failed")
            })
            .and_then(|network| run(&network, request, progress, &cancel));
        finished.store(true, Ordering::Release);
        result
    });
    // The registered server gets its own worker later; this one must be gone.
    if !matches!(
        worker.shutdown(Duration::from_secs(60)),
        Ok(crate::worker::WorkerShutdown::Complete)
    ) {
        return Err("install_failed");
    }
    // Whatever stopped after cancellation was requested is a cancellation.
    match result {
        Err(_) if cancel.load(Ordering::Acquire) || progress.cancelled() => Err("cancelled"),
        result => result,
    }
}

/// The installation id the software service would record: the installer's
/// instance directory, when the trusted artifacts show it.
fn installation_id(request: &PackInstallRequest<'_>) -> String {
    let instances = request.server_dir.join("install").join("instances");
    request
        .installed
        .artifacts
        .iter()
        .find_map(|a| {
            let rest = a.path.strip_prefix(&instances).ok()?;
            let id = rest.components().next()?.as_os_str().to_str()?;
            (!id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
            .then(|| id.to_owned())
        })
        .unwrap_or_else(|| "created".into())
}

fn failed(error: &crate::packs::ReleaseError) -> &'static str {
    match error.code() {
        "content_provider_unavailable"
        | "content_pack_download_failed"
        | "content_pack_download_unavailable" => "download_failed",
        "content_pack_hash_mismatch" | "pack_release_conflict" => "integrity_conflict",
        _ => "install_failed",
    }
}

fn run(
    network: &crate::host::NetworkHost,
    request: &PackInstallRequest<'_>,
    progress: &dyn CreateProgress,
    cancel: &Arc<AtomicBool>,
) -> Result<(), &'static str> {
    let subject = &request.subject;
    let review = request.review;
    let stopped = || cancel.load(Ordering::Acquire);
    let workspace = PackWorkspace::open(network.database())
        .map(|w| {
            w.with_download_settings(network.configuration().downloads())
                .with_cancel(cancel.clone())
        })
        .map_err(|_| "install_failed")?;
    let software = network.software().cloned().ok_or("install_failed")?;
    // The base software is the pack's own loader, already verified and
    // installed by this create: record it exactly as the software service
    // would, so the pack install needs no software switch.
    let installed = request.installed;
    software
        .record_created_installation(
            subject,
            crate::software::Installation {
                custom_jar: None,
                loader_version: installed.loader_version.clone(),
                software: Some(installed.software),
                version: Some(installed.version.clone()),
                build: installed.build,
                installation_id: installation_id(request),
            },
            installed.artifacts.clone(),
        )
        .map_err(|_| "install_failed")?;
    let tag = request.server_id.simple().to_string();
    progress.phase("downloading_software");
    let source = match (request.source, request.upload) {
        (
            PackSourceRequest::Provider {
                provider,
                project_id,
                version_id,
            },
            None,
        ) => InstallSource::Provider {
            provider: provider.clone(),
            project_id: project_id.clone(),
            version_id: version_id.clone(),
        },
        (PackSourceRequest::Upload { .. }, Some(path)) => {
            let upload_id = format!("create-{tag}");
            upload(&workspace, subject, path, &upload_id, &tag, review, progress)?;
            InstallSource::UploadedArchive { upload_id }
        }
        _ => return Err("install_failed"),
    };
    if stopped() {
        return Err("cancelled");
    }
    let providers = UreqProviderClient::configured();
    let plan = workspace
        .install_plan(
            subject,
            &review.minecraft_version,
            Some(&review.loader),
            &crate::packs::workspace::PackInstallRequest {
                analyze: false,
                source,
            },
            network.files(),
            network.releases(),
            &providers,
        )
        .map_err(|e| failed(&e))?;
    // Exactly the reviewed pack: its own Minecraft version and loader.
    let loader = if plan.software == "none" {
        "vanilla"
    } else {
        plan.software.as_str()
    };
    if plan.minecraft_version != review.minecraft_version
        || loader != review.loader
        || plan.loader_version != review.loader_version
    {
        return Err("integrity_conflict");
    }
    // The recorded base software is exactly the pack's: a switch would install
    // software a second time, which a create never does.
    if !plan.valid || plan.software_switch_required {
        return Err("install_failed");
    }
    if stopped() {
        return Err("cancelled");
    }
    let input = PackInstallApply {
        // The owner acknowledged the pack review's notices at create apply.
        acknowledge_warnings: true,
        // Never a software switch: the base software is already the pack's.
        acknowledge_software_switch: false,
        plan_id: plan.plan_id.clone(),
        confirmation_token: plan.confirmation_token.clone(),
        choices: Vec::new(),
        set_as_modpack: false,
    };
    let pack = crate::software::PackWork {
        workspace: workspace.clone(),
        files: network.files().clone(),
        input,
        key: format!("create-pack-{tag}"),
    };
    progress.phase("installing");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "install_failed")?;
    let job = runtime
        .block_on(software.apply_pack(subject, pack))
        .map_err(|_| "install_failed")?;
    let _ = workspace.track_install_job(subject, &job.id);
    // The software service runs the job on its own worker thread. After a
    // cancellation request the job stops at the pack engine's next safe
    // boundary; wait for it to settle so nothing writes into the server after
    // the create removes it.
    let deadline = std::time::Instant::now() + Duration::from_secs(3600);
    loop {
        let state = software
            .jobs(subject)
            .map_err(|_| "install_failed")?
            .into_iter()
            .find(|j| j.id == job.id)
            .ok_or("install_failed")?;
        match state.state.as_str() {
            "succeeded" => break,
            "failed" | "cancelled" => {
                return Err(match state.error.as_deref() {
                    Some(e) if e.contains("download") => "download_failed",
                    Some(e) if e.contains("hash") || e.contains("integrity") => {
                        "integrity_conflict"
                    }
                    _ => "install_failed",
                });
            }
            _ => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err("install_failed");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let _ = workspace.install_finish(subject, &plan.plan_id);
    Ok(())
}

/// Copy the host-private uploaded archive into the server's own pack upload
/// store, re-checking that it is still exactly the reviewed archive.
fn upload(
    workspace: &PackWorkspace,
    subject: &crate::identity::Subject,
    path: &Path,
    upload_id: &str,
    tag: &str,
    review: &crate::packs::workspace::CreatePackReview,
    progress: &dyn CreateProgress,
) -> Result<(), &'static str> {
    use sha2::Digest;
    let open = || std::fs::File::open(path).map_err(|_| "install_failed");
    let total = open()?.metadata().map_err(|_| "install_failed")?.len();
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut open()?, &mut hasher).map_err(|_| "install_failed")?;
    let sha256 = hex::encode(hasher.finalize());
    if sha256 != review.source_digest {
        return Err("integrity_conflict");
    }
    let fail = |_| "install_failed";
    workspace
        .upload_begin(
            subject,
            &format!("create-upload-begin-{tag}"),
            &UploadBegin {
                upload_id: upload_id.to_owned(),
                total_size: total.to_string(),
                sha256,
                archive: true,
            },
        )
        .map_err(fail)?;
    let mut reader = open()?;
    let mut offset = 0u64;
    let mut chunk = vec![0u8; 512 * 1024];
    for index in 0.. {
        let n = reader.read(&mut chunk).map_err(|_| "install_failed")?;
        if n == 0 {
            break;
        }
        if progress.cancelled() {
            return Err("cancelled");
        }
        workspace
            .upload_chunk(
                subject,
                &format!("create-upload-{index}-{tag}"),
                &UploadChunk {
                    upload_id: upload_id.to_owned(),
                    offset: offset.to_string(),
                    content_base64: STANDARD.encode(&chunk[..n]),
                },
            )
            .map_err(fail)?;
        offset += n as u64;
        progress.bytes(offset, Some(total));
    }
    workspace
        .upload_finish(subject, &format!("create-upload-finish-{tag}"), upload_id)
        .map_err(fail)?;
    Ok(())
}
