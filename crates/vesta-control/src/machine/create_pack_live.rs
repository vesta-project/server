//! Live pack content install for a modpack create. The new server is not
//! registered yet: its own worker is opened briefly from the trusted
//! `host.toml`, the owner's policy is synchronized to it, and the server's own
//! pack engine runs exactly as for an existing server (install plan, then
//! `apply_pack`, which installs the pack's loader through the server's software
//! service and verifies every pack file). The worker is shut down before the
//! create job registers the server.
use super::{CreateProgress, PackInstallRequest, PackSourceRequest};
use crate::{
    config::HostConfiguration,
    content::UreqProviderClient,
    packs::workspace::{InstallSource, PackInstallApply, PackWorkspace, UploadBegin, UploadChunk},
    worker::{ServerWorker, Worker},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{io::Read, path::Path, sync::Arc, time::Duration};

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
    let result = handle
        .sync_policy(request.policy.clone())
        .map_err(|_| "install_failed")
        .and_then(|()| {
            handle
                .authorize(request.server_subject.clone())
                .map_err(|_| "install_failed")
        })
        .and_then(|network| run(&network, request, progress));
    // The registered server gets its own worker later; this one must be gone.
    if !matches!(
        worker.shutdown(Duration::from_secs(60)),
        Ok(crate::worker::WorkerShutdown::Complete)
    ) {
        return Err("install_failed");
    }
    result
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
) -> Result<(), &'static str> {
    let subject = &request.subject;
    let review = request.review;
    let workspace = PackWorkspace::open(network.database())
        .map(|w| w.with_download_settings(network.configuration().downloads()))
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
    if progress.cancelled() {
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
    if !plan.valid {
        return Err("install_failed");
    }
    let software = network.software().cloned().ok_or("install_failed")?;
    let input = PackInstallApply {
        // The owner acknowledged the pack review's notices at create apply.
        acknowledge_warnings: true,
        acknowledge_software_switch: true,
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
    // The software service runs the job on its own worker thread.
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
