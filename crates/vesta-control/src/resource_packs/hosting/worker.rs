use super::*;
use crate::{
    content::{
        Provider, ProviderClient, UreqDownloadHttp, UreqProviderClient, fetch_verified_source,
    },
    packs::workspace::{EntryKind, WorkspaceProvider},
};
use std::time::{Duration, Instant};

pub(super) fn run(inner: Arc<Inner>) {
    loop {
        let work = {
            let Ok(mut queue) = inner.queue.lock() else {
                return;
            };
            while queue.jobs.is_empty() && !queue.stop {
                queue = match inner.wake.wait(queue) {
                    Ok(q) => q,
                    Err(_) => return,
                };
            }
            if queue.stop {
                return;
            }
            queue.jobs.pop_front().expect("nonempty queue")
        };
        let mut job = work.job;
        let outcome = execute(&inner, &work.actor, &work.draft, &mut job);
        job.state = if outcome.is_ok() {
            "completed"
        } else {
            "failed"
        }
        .into();
        job.step = "finished".into();
        job.updated_at_ms = inner.db.clock().millis();
        match outcome {
            Ok(release) => job.release = Some(release),
            Err(e) => job.error = Some(e.code().into()),
        }
        // If the final transaction fails, the durable running job is marked
        // interrupted on reopen. No automatic re-download or activation follows.
        let _ = inner
            .db
            .write_with(|tx| store::put_job(tx.connection(), &job));
    }
}
fn step(inner: &Inner, actor: &Subject, job: &mut Job, value: &str) -> Result<()> {
    if inner.cancel.load(Ordering::Acquire) {
        return Err(Error::Unavailable);
    }
    job.state = "running".into();
    job.step = value.into();
    job.updated_at_ms = inner.db.clock().millis();
    inner
        .db
        .write_with(|tx| {
            store::authorize(tx.connection(), &inner.db, actor)?;
            store::put_job(tx.connection(), job)
        })
        .map_err(Into::into)
}
fn execute(inner: &Inner, actor: &Subject, draft: &Draft, job: &mut Job) -> Result<Release> {
    let settings = draft.settings.as_ref().ok_or(Error::Invalid)?;
    let deadline = Instant::now() + Duration::from_secs(120);
    step(inner, actor, job, "resolving")?;
    let provider = UreqProviderClient::configured();
    let http = UreqDownloadHttp::default();
    let mut inputs = Vec::new();
    for layer in &settings.layers {
        step(inner, actor, job, "preparing_inputs")?;
        if Instant::now() >= deadline {
            return Err(Error::Build(super::super::Error::TimedOut));
        }
        let sha = match &layer.source {
            Source::Upload { sha256 } => sha256.clone(),
            Source::Provider {
                provider: platform,
                project_id,
                version_id,
            } => {
                let selection =
                    hash(store::encode(&(&layer.source, &settings.minecraft_version))?.as_bytes());
                let cached: Option<String> = inner.db.read(|c| {
                    Ok(c.query_row(
                        "SELECT sha256 FROM hosted_pack_sources WHERE selection_sha256=?",
                        [&selection],
                        |r| r.get(0),
                    )
                    .optional()?)
                })?;
                if let Some(sha) = cached {
                    let file = inner
                        .db
                        .directory()
                        .uploads()?
                        .open(&uploads::blob_name(&sha)?)?
                        .ok_or(Error::NotFound)?;
                    inputs.push(Input::new(layer.id.clone(), file, sha)?);
                    continue;
                }
                // Metadata adapter reuses the canonical provider identity/kind/
                // Minecraft-version checks, never accepting caller URLs or hashes.
                let meta = provider
                    .with_deadline(deadline)
                    .map_err(|_| Error::Unavailable)?
                    .pack_metadata(platform, project_id, version_id)
                    .map_err(|_| Error::Unavailable)?;
                if meta.kind != EntryKind::ResourcePack
                    || !meta.downloadable
                    || !meta.client_supported
                    || !meta
                        .minecraft_versions
                        .contains(&settings.minecraft_version)
                {
                    return Err(Error::Invalid);
                }
                let platform = if platform == "modrinth" {
                    Provider::Modrinth
                } else {
                    Provider::Curseforge
                };
                let source = provider
                    .resolve_with_deadline(platform, project_id, version_id, deadline)
                    .map_err(|_| Error::Unavailable)?;
                if !source.filename.to_ascii_lowercase().ends_with(".zip") {
                    return Err(Error::Invalid);
                }
                let bytes = fetch_verified_source(&source, &http, deadline)
                    .map_err(|_| Error::Unavailable)?;
                let sha = uploads::store_bytes(inner, &bytes)?;
                inner.db.write(|c| {
                    c.execute(
                        "INSERT OR IGNORE INTO hosted_pack_sources VALUES(?,?)",
                        params![selection, sha],
                    )?;
                    Ok(())
                })?;
                sha
            }
        };
        let dir = inner.db.directory().uploads()?;
        let file = dir
            .open(&uploads::blob_name(&sha)?)?
            .ok_or(Error::NotFound)?;
        inputs.push(Input::new(layer.id.clone(), file, sha)?);
    }
    step(inner, actor, job, "building")?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Error::Build(super::super::Error::TimedOut));
    }
    let built = super::super::build(
        inner.db.directory(),
        inputs,
        settings.format,
        &settings.description,
        BuildLimits {
            time: remaining,
            ..BuildLimits::default()
        },
    )?;
    step(inner, actor, job, "publishing")?;
    if settings.minecraft_version != inner.hooks.minecraft_version()? {
        return Err(Error::TargetChanged);
    }
    // Re-check the draft before publication; immutable files may remain after a
    // later SQL failure, but never become publicly addressable without a record.
    inner.db.read_with(|tx| -> Result<()> {
        store::authorize(tx.connection(), &inner.db, actor)?;
        if store::draft(tx.connection())?.revision != draft.revision {
            return Err(Error::Conflict);
        }
        Ok(())
    })?;
    let artifact = built.publish()?.compact_report();
    let release = Release {
        id: job.id.clone(),
        draft_revision: draft.revision.clone(),
        pack_id: draft.pack_id.clone(),
        url: format!(
            "{}/resource-packs/{}.zip",
            origin(&settings.public_origin)?,
            artifact.sha256()
        ),
        settings: settings.clone(),
        artifact,
    };
    inner.db.write_with(|tx| -> Result<()> {
        let c = tx.connection();
        store::authorize(c, &inner.db, actor)?;
        if store::draft(c)?.revision != draft.revision {
            return Err(Error::Conflict);
        }
        let count: i64 = c.query_row("SELECT count(*) FROM hosted_pack_releases", [], |r| {
            r.get(0)
        })?;
        if count >= 64 {
            return Err(Error::Limit);
        }
        c.execute(
            "INSERT INTO hosted_pack_releases VALUES(?,?)",
            params![release.id, store::encode(&release)?],
        )?;
        store::audit(c, &inner.audit, actor, "resource_pack.build")?;
        Ok(())
    })?;
    Ok(release)
}
