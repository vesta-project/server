//! Staged processing and receipt-last publication shared by both Forge families.
use super::*;
use crate::{
    install as publication, InstallOutcome, InstallPlan, ManagedArtifact, ManagedLaunch, Receipt,
    ServerInstaller,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::sync::{Arc, OnceLock};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vesta_game_runtime::process::tree::ProcessTree;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessedReceipt {
    pub schema: u32,
    pub install_id: String,
    pub plan_hash: String,
    pub software: Software,
    pub version: String,
    pub loader_version: String,
    pub files: Vec<GeneratedFile>,
    pub inputs: Vec<crate::bundle::ArtifactRecord>,
    pub java_requirement: JavaRequirement,
    pub launch: LaunchRecipe,
    pub completed_at: u64,
}
impl ProcessedReceipt {
    pub(crate) fn into_receipt(self) -> Result<Receipt> {
        if self.schema != 3
            || !matches!(self.software, Software::Forge | Software::NeoForge)
            || self.files.len() > MAX_FILES
            || self.inputs.is_empty()
            || self.inputs.len() > MAX_FILES
        {
            return Err(InstallError::ReceiptInvalid);
        }
        let first = &self.inputs[0];
        Ok(Receipt {
            schema: 3,
            install_id: self.install_id.clone(),
            plan_hash: self.plan_hash.clone(),
            software: self.software,
            version: self.version.clone(),
            build: None,
            published_digest: first.published_digest.clone(),
            published_size: Some(first.published_size),
            actual_size: first.actual_size,
            observed_sha256: first.observed_sha256.clone(),
            java_requirement: self.java_requirement.clone(),
            launch: self.launch.clone(),
            eula: "not_accepted".into(),
            warnings: vec![Warning::LegacyChecksum, Warning::FirstRunEffects],
            completed_at: self.completed_at,
            bundle: None,
            processed: Some(self),
        })
    }
}
fn plan(value: &InstallPlan) -> Result<&ProcessorPlan> {
    if value.schema != 3
        || value.bundle.is_some()
        || value.artifact.is_some()
        || !matches!(value.software, Software::Forge | Software::NeoForge)
    {
        return Err(InstallError::InvalidInput);
    }
    let plan = value.processed.as_ref().ok_or(InstallError::InvalidInput)?;
    if plan.artifacts.len() > MAX_FILES
        || plan.steps.is_empty() && plan.legacy.is_none()
        || plan.steps.len() > MAX_PROCESSORS
    {
        return Err(InstallError::LimitExceeded);
    }
    if plan.disk_cap == 0 || plan.disk_cap > value.limits.total_bytes {
        return Err(InstallError::InvalidInput);
    }
    Ok(plan)
}
fn parent(path: &Path) -> Result<()> {
    let directory = path.parent().ok_or(InstallError::PathRefused)?;
    if !directory.exists() {
        parent(directory)?;
        publication::child_directory(directory)?;
    }
    publication::guard(directory)
}
pub(super) async fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    parent(path)?;
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    file.write_all(bytes)
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    file.sync_all()
        .await
        .map_err(|_| InstallError::PublishFailed)
}
pub(super) async fn digest(path: &Path, cap: u64) -> Result<(u64, String)> {
    publication::plain_file(path)?;
    let before = std::fs::metadata(path).map_err(|_| InstallError::ArtifactChanged)?;
    if before.len() > cap {
        return Err(InstallError::LimitExceeded);
    }
    let opened = std::fs::File::open(path).map_err(|_| InstallError::ArtifactChanged)?;
    let identity = crate::execution_workspace::file_identity_with_links(&opened, 1)?;
    let mut file = tokio::fs::File::from_std(opened);
    let mut sha = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|_| InstallError::ArtifactChanged)?;
        if n == 0 {
            break;
        }
        size = size
            .checked_add(n as u64)
            .ok_or(InstallError::LimitExceeded)?;
        if size > cap {
            return Err(InstallError::LimitExceeded);
        }
        sha.update(&buffer[..n]);
    }
    publication::plain_file(path)?;
    let opened = file.into_std().await;
    let current = std::fs::File::open(path).map_err(|_| InstallError::ArtifactChanged)?;
    if crate::execution_workspace::file_identity_with_links(&opened, 1)? != identity
        || crate::execution_workspace::file_identity_with_links(&current, 1)? != identity
    {
        return Err(InstallError::ArtifactChanged);
    }
    let after = std::fs::metadata(path).map_err(|_| InstallError::ArtifactChanged)?;
    if size != before.len()
        || before.modified().ok() != after.modified().ok()
        || size != after.len()
    {
        return Err(InstallError::ArtifactChanged);
    }
    Ok((size, format!("{:x}", sha.finalize())))
}
async fn inputs(
    http: &Transport,
    plan: &ProcessorPlan,
    root: &Path,
    job: &mut Job,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<crate::bundle::ArtifactRecord>> {
    let mut records = Vec::new();
    for item in plan.artifacts.iter().chain(plan.mappings.iter()) {
        relative(&item.destination)?;
        let path = root.join(&item.destination);
        parent(&path)?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .map_err(|_| InstallError::PublishFailed)?;
        let received = http
            .family_artifact(&item.pin, item.family, job, reporter, &mut file)
            .await?;
        file.sync_all()
            .await
            .map_err(|_| InstallError::PublishFailed)?;
        records.push(crate::bundle::ArtifactRecord {
            path: format!("software/{}", item.destination),
            url: item.pin.url.clone(),
            family: item.family,
            published_size: item.pin.size.ok_or(InstallError::InvalidMetadata)?,
            published_digest: item.pin.digest.clone(),
            actual_size: received.size,
            observed_sha256: received.observed_sha256,
        });
    }
    Ok(records)
}
async fn verify_inputs(root: &Path, records: &[crate::bundle::ArtifactRecord]) -> Result<()> {
    for record in records {
        let path = root.join(
            record
                .path
                .strip_prefix("software/")
                .ok_or(InstallError::ReceiptInvalid)?,
        );
        let pin = ArtifactPin {
            url: record.url.clone(),
            digest: record.published_digest.clone(),
            size: Some(record.published_size),
        };
        crate::bundle::rehash(&path, &pin, record.actual_size, &record.observed_sha256, 1).await?;
    }
    Ok(())
}
fn scoped(root: &Path, value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if !path.is_absolute() || !path.starts_with(root) {
        return Err(InstallError::PathRefused);
    }
    publication::guard(&path)?;
    Ok(path)
}
async fn extraction(plan: &ProcessorPlan, step: &Step, root: &Path) -> Result<()> {
    let mut i = 0;
    while i < step.args.len() {
        match step.args[i].as_str() {
            "--from" => {
                let source = step.args.get(i + 1).ok_or(InstallError::InvalidMetadata)?;
                if step.args.get(i + 2).map(String::as_str) != Some("--to") {
                    return Err(InstallError::InvalidMetadata);
                }
                let target = recipe::resolve(
                    plan,
                    step.args.get(i + 3).ok_or(InstallError::InvalidMetadata)?,
                    root,
                )?;
                let target = scoped(root, &target)?;
                let data = plan
                    .embedded
                    .get(source)
                    .ok_or(InstallError::InvalidMetadata)?;
                write(&target, data).await?;
                i += 4;
            }
            "--exec" | "--optional" => {
                i += 2;
            }
            "--task" | "--archive" => {
                i += 2;
            }
            _ => return Err(InstallError::InvalidMetadata),
        }
    }
    Ok(())
}
struct Running {
    child: Child,
    tree: ProcessTree,
    confirmed: bool,
}
impl Drop for Running {
    fn drop(&mut self) {
        if !self.confirmed {
            let _ = self.tree.kill(&mut self.child);
            let _ = self.child.kill();
            // Keep the exact child owned through a bounded cleanup attempt, never look up a PID.
            let until = std::time::Instant::now() + Duration::from_secs(2);
            while std::time::Instant::now() < until {
                if self.child.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
async fn processor(
    runtime: &crate::runtime::RuntimeBinding,
    root: &Path,
    step: &Step,
    args: Vec<String>,
    deadline: tokio::time::Instant,
    reporter: &dyn ProgressReporter,
    disk_cap: u64,
) -> Result<()> {
    let cp = step
        .classpath
        .iter()
        .chain(std::iter::once(&step.jar))
        .map(|s| {
            root.join(s)
                .to_str()
                .map(str::to_owned)
                .ok_or(InstallError::PathRefused)
        })
        .collect::<Result<Vec<_>>>()?;
    let separator = if cfg!(windows) { ';' } else { ':' };
    if cp.iter().any(|s| s.contains(separator)) {
        return Err(InstallError::PathRefused);
    }
    let mut command = Command::new(&runtime.path);
    let temporary = root.parent().ok_or(InstallError::PathRefused)?.join("tmp");
    publication::child_directory(&temporary)?;
    let mut invocation = vec![
        "-Xmx512m".to_owned(),
        "-Djava.awt.headless=true".to_owned(),
        format!("-Djava.io.tmpdir={}", temporary.display()),
        format!("-Duser.home={}", temporary.display()),
    ];
    if step.main.is_empty() {
        if step.args != ["--onlyCheckJava"] || !step.jar.ends_with("-shim.jar") {
            return Err(InstallError::InvalidInput);
        }
        invocation.push("-jar".into());
        invocation.push(
            root.join(&step.jar)
                .to_str()
                .ok_or(InstallError::PathRefused)?
                .into(),
        );
    } else {
        invocation.extend([
            "-cp".into(),
            cp.join(&separator.to_string()),
            step.main.clone(),
        ]);
    }
    invocation.extend(args);
    let invocation_file =
        temporary.join(format!("processor-{}.args", uuid::Uuid::new_v4().simple()));
    write(&invocation_file, &recipe::argfile(&invocation)?).await?;
    if runtime.major >= 9 {
        command.arg(format!(
            "@{}",
            invocation_file.to_str().ok_or(InstallError::PathRefused)?
        ));
    } else {
        // Java 8 has no @argfile support; processor classpaths are bounded.
        let bytes = invocation
            .iter()
            .map(|s| s.encode_utf16().count().saturating_mul(2).saturating_add(4))
            .sum::<usize>();
        if bytes > 30_000 {
            return Err(InstallError::LimitExceeded);
        }
        command.args(&invocation);
    }
    command
        .env("TMP", &temporary)
        .env("TEMP", &temporary)
        .env("TMPDIR", &temporary)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in [
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "OPENJ9_JAVA_OPTIONS",
        "IBM_JAVA_OPTIONS",
    ] {
        command.env_remove(key);
    }
    let tree = ProcessTree::prepare().map_err(|_| InstallError::RuntimeFailed)?;
    ProcessTree::configure(&mut command);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }
    let child = command.spawn().map_err(|_| InstallError::RuntimeFailed)?;
    let mut running = Running {
        child,
        tree,
        confirmed: false,
    };
    running
        .tree
        .attach(&running.child)
        .map_err(|_| InstallError::RuntimeFailed)?;
    let out = running
        .child
        .stdout
        .take()
        .ok_or(InstallError::RuntimeFailed)?;
    let err = running
        .child
        .stderr
        .take()
        .ok_or(InstallError::RuntimeFailed)?;
    let drain = |mut pipe: Box<dyn Read + Send>, path: PathBuf| {
        tokio::task::spawn_blocking(move || {
            let mut log = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|_| InstallError::RuntimeFailed)?;
            let mut buffer = [0u8; 8192];
            let mut total = 0usize;
            loop {
                let n = pipe
                    .read(&mut buffer)
                    .map_err(|_| InstallError::RuntimeFailed)?;
                if n == 0 {
                    return Ok(());
                }
                total += n;
                if total > 1024 * 1024 {
                    return Err(InstallError::LimitExceeded);
                }
                std::io::Write::write_all(&mut log, &buffer[..n])
                    .map_err(|_| InstallError::RuntimeFailed)?;
            }
        })
    };
    let mut stdout = Some(drain(
        Box::new(out),
        invocation_file.with_extension("stdout.log"),
    ));
    let mut stderr = Some(drain(
        Box::new(err),
        invocation_file.with_extension("stderr.log"),
    ));
    let mut measure_at = tokio::time::Instant::now();
    loop {
        crate::execution::check(deadline, reporter)?;
        if tokio::time::Instant::now() >= measure_at {
            measure(root.parent().ok_or(InstallError::PathRefused)?, disk_cap)?;
            measure_at = tokio::time::Instant::now() + Duration::from_secs(1);
        }
        for output in [&mut stdout, &mut stderr] {
            if output.as_ref().is_some_and(|task| task.is_finished()) {
                output
                    .take()
                    .ok_or(InstallError::RuntimeFailed)?
                    .await
                    .map_err(|_| InstallError::RuntimeFailed)??;
            }
        }
        if let Some(status) = vesta_game_runtime::process::tree::observe(&mut running.child)
            .map_err(|_| InstallError::RuntimeFailed)?
        {
            // On Unix observe keeps the group leader unreaped until the family kill.
            running
                .tree
                .kill(&mut running.child)
                .map_err(|_| InstallError::RuntimeFailed)?;
            running
                .child
                .wait()
                .map_err(|_| InstallError::RuntimeFailed)?;
            let drains = async {
                for output in [stdout.take(), stderr.take()].into_iter().flatten() {
                    output.await.map_err(|_| InstallError::RuntimeFailed)??;
                }
                Ok(())
            };
            crate::transfer::controlled(drains, deadline, reporter).await?;
            running.confirmed = true;
            let _ = std::fs::remove_file(&invocation_file);
            return if status.success() {
                Ok(())
            } else {
                Err(InstallError::RuntimeFailed)
            };
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
async fn steps(
    value: &InstallPlan,
    root: &Path,
    job: &Job,
    reporter: &dyn ProgressReporter,
) -> Result<()> {
    let plan = plan(value)?;
    for (index, step) in plan.steps.iter().enumerate() {
        job.ready(reporter).await?;
        value.runtime.recheck()?;
        reporter.start_step("Preparing modded server", Some((index + 1) as u32));
        if step.args.iter().any(|a| a == "EXTRACT_FILES") {
            extraction(plan, step, root).await?;
            continue;
        }
        if step.args.iter().any(|a| a == "DOWNLOAD_MOJMAPS") {
            let output = step
                .args
                .windows(2)
                .find(|v| v[0] == "--output")
                .ok_or(InstallError::InvalidMetadata)?;
            let path = scoped(root, &recipe::resolve(plan, &output[1], root)?)?;
            let bytes = tokio::fs::read(root.join("server-mappings.txt"))
                .await
                .map_err(|_| InstallError::ArtifactChanged)?;
            write(&path, &bytes).await?;
            continue;
        }
        let args = step
            .args
            .iter()
            .map(|a| recipe::resolve(plan, a, root))
            .collect::<Result<Vec<_>>>()?;
        for arg in &args {
            if Path::new(arg).is_absolute() {
                scoped(root, arg)?;
            }
            if arg.contains("../") || arg.contains("..\\") {
                return Err(InstallError::PathRefused);
            }
        }
        for pair in args.windows(2) {
            if matches!(
                pair[0].as_str(),
                "--input"
                    | "--output"
                    | "--left"
                    | "--right"
                    | "--slim"
                    | "--extra"
                    | "--srg"
                    | "--names"
                    | "--clean"
                    | "--apply"
                    | "--extract-libraries-to"
                    | "--apply-patches"
                    | "--in-jar"
                    | "--out-jar"
                    | "--srg-in"
            ) {
                scoped(root, &pair[1])?;
            }
        }
        processor(
            &value.runtime,
            root,
            step,
            args,
            job.deadline,
            reporter,
            plan.disk_cap,
        )
        .await?;
        for (path, expected) in &step.outputs {
            let path = scoped(root, &recipe::resolve(plan, path, root)?)?;
            let expected = recipe::resolve(plan, expected, root)?;
            publication::plain_file(&path)?;
            let mut file = tokio::fs::File::open(&path)
                .await
                .map_err(|_| InstallError::ArtifactChanged)?;
            use sha1::{Digest as _, Sha1};
            let mut sha = Sha1::new();
            let mut bytes = 0u64;
            let mut buffer = [0u8; 65536];
            loop {
                job.ready(reporter).await?;
                let n = file
                    .read(&mut buffer)
                    .await
                    .map_err(|_| InstallError::ArtifactChanged)?;
                if n == 0 {
                    break;
                }
                bytes += n as u64;
                if bytes > job.limits.artifact_bytes {
                    return Err(InstallError::LimitExceeded);
                }
                sha.update(&buffer[..n]);
            }
            if format!("{:x}", sha.finalize()) != expected {
                return Err(InstallError::ArtifactChanged);
            }
        }
    }
    Ok(())
}
async fn verify_layout(plan: &ProcessorPlan, root: &Path, cap: u64) -> Result<()> {
    if plan.legacy.is_some() {
        return legacy::verify_layout(plan, root, cap).await;
    }
    let patched = scoped(root, &recipe::resolve(plan, "{PATCHED}", root)?)?;
    publication::plain_file(&patched)?;
    if std::fs::metadata(&patched)
        .map_err(|_| InstallError::ArtifactChanged)?
        .len()
        > cap
    {
        return Err(InstallError::LimitExceeded);
    }
    let jar = std::fs::File::open(&patched).map_err(|_| InstallError::ArtifactChanged)?;
    let archive = zip::ZipArchive::new(jar).map_err(|_| InstallError::ArtifactChanged)?;
    if archive.is_empty() || archive.len() > 100_000 {
        return Err(InstallError::ArtifactChanged);
    }
    for token in &plan.launch {
        for component in token.split(if cfg!(windows) { ';' } else { ':' }) {
            if let Some(start) = component.find("libraries/") {
                let path = &component[start..];
                relative(path)?;
                publication::plain_file(&root.join(path))?;
            }
        }
    }
    for (path, expected) in &plan.shim_libraries {
        let (_, hash) = digest(&root.join(path), cap).await?;
        if &hash != expected {
            return Err(InstallError::ArtifactChanged);
        }
    }
    Ok(())
}
fn inventory(root: &Path, cap: u64) -> Result<Vec<PathBuf>> {
    let mut todo = vec![root.to_owned()];
    let mut files = Vec::new();
    let mut entries = 0;
    let mut bytes = 0u64;
    while let Some(dir) = todo.pop() {
        publication::guard(&dir)?;
        for entry in std::fs::read_dir(dir).map_err(|_| InstallError::PathRefused)? {
            let path = entry.map_err(|_| InstallError::PathRefused)?.path();
            publication::guard(&path)?;
            entries += 1;
            if entries > MAX_FILES * 4 {
                return Err(InstallError::LimitExceeded);
            }
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|_| InstallError::PathRefused)?;
            if metadata.is_dir() {
                todo.push(path);
            } else if metadata.is_file() {
                let name = path
                    .strip_prefix(root)
                    .map_err(|_| InstallError::PathRefused)?
                    .to_str()
                    .ok_or(InstallError::PathRefused)?
                    .replace('\\', "/");
                relative(&name)?;
                if !(name.starts_with("libraries/")
                    || name.starts_with("data/")
                    || name.starts_with("minecraft_server.") && name.ends_with(".jar")
                    || matches!(
                        name.as_str(),
                        "minecraft.jar"
                            | "installer.jar"
                            | "server-mappings.txt"
                            | "run.sh"
                            | "run.bat"
                            | "user_jvm_args.txt"
                            | "README.txt"
                            | "launch.args"
                            | "launch.jar"
                    ))
                {
                    return Err(InstallError::PathRefused);
                }
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or(InstallError::LimitExceeded)?;
                if bytes > cap || files.len() >= MAX_FILES {
                    return Err(InstallError::LimitExceeded);
                }
                files.push(path);
            } else {
                return Err(InstallError::PathRefused);
            }
        }
    }
    files.sort();
    Ok(files)
}
fn measure(root: &Path, cap: u64) -> Result<()> {
    let mut todo = vec![root.to_owned()];
    let mut bytes = 0u64;
    let mut count = 0;
    while let Some(directory) = todo.pop() {
        publication::guard(&directory)?;
        for item in std::fs::read_dir(directory).map_err(|_| InstallError::PathRefused)? {
            let path = item.map_err(|_| InstallError::PathRefused)?.path();
            publication::guard(&path)?;
            count += 1;
            if count > MAX_FILES * 4 {
                return Err(InstallError::LimitExceeded);
            }
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|_| InstallError::PathRefused)?;
            if metadata.is_dir() {
                todo.push(path);
            } else if metadata.is_file() {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or(InstallError::LimitExceeded)?;
                if bytes > cap {
                    return Err(InstallError::LimitExceeded);
                }
            } else {
                return Err(InstallError::PathRefused);
            }
        }
    }
    Ok(())
}
pub(crate) async fn execute(
    installer: &ServerInstaller,
    value: &InstallPlan,
    reporter: &dyn ProgressReporter,
) -> Result<InstallOutcome> {
    let recipe = plan(value)?;
    // Processor jobs are CPU/memory heavy even though the agent is not. One
    // Forge-family installation per agent process leaves room for control and Minecraft.
    static PROCESSORS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let _processor_slot = PROCESSORS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| InstallError::Busy)?;
    let _slot = installer.coordinator.slot()?;
    let (volume, free) = installer.space()?;
    let reservation = installer.coordinator.reserve(
        volume,
        free,
        recipe
            .disk_cap
            .checked_mul(2)
            .ok_or(InstallError::LimitExceeded)?,
        value.limits.reserve_bytes,
    )?;
    let stages = value.base.join(".staging");
    publication::child_directory(&stages)?;
    let stage = stages.join(format!("forge_{}", uuid::Uuid::new_v4().simple()));
    publication::child_directory(&stage)?;
    // Retain an incomplete stage on every error; never recursively delete unknown processor effects.
    let root = stage.join("work");
    publication::child_directory(&root)?;
    let mut job = Job::new(value.limits, value.transferred);
    let records = inputs(&installer.http, recipe, &root, &mut job, reporter).await?;
    if recipe.legacy.is_some() {
        legacy::materialize(recipe, &root).await?;
    }
    for (path, bytes) in &recipe.embedded {
        if path.starts_with("data/") {
            write(&root.join(path), bytes).await?;
        }
    }
    verify_inputs(&root, &records).await?;
    steps(value, &root, &job, reporter).await?;
    verify_inputs(&root, &records).await?;
    verify_layout(recipe, &root, value.limits.artifact_bytes).await?;
    if recipe.legacy.is_some() {
        write(
            &root.join("launch.jar"),
            &legacy::pathing_jar(recipe, &value.destination().join("software"))?,
        )
        .await?;
    }
    // Generate absolute launch paths for the final immutable install, not the staging cwd.
    let launch = recipe::argfile(&recipe::launch(
        recipe,
        &value.destination().join("software"),
    )?)?;
    write(&root.join("launch.args"), &launch).await?;
    let paths = inventory(&root, recipe.disk_cap)?;
    let mut files = Vec::new();
    for path in &paths {
        job.ready(reporter).await?;
        let (size, hash) = digest(path, value.limits.artifact_bytes).await?;
        files.push(GeneratedFile {
            path: format!(
                "software/{}",
                path.strip_prefix(&root)
                    .map_err(|_| InstallError::PathRefused)?
                    .to_str()
                    .ok_or(InstallError::PathRefused)?
                    .replace('\\', "/")
            ),
            size,
            sha256: hash,
        });
    }
    installer.roots.recheck()?;
    value.runtime.recheck()?;
    job.ready(reporter).await?;
    let (volume, free) = installer.space()?;
    reservation.recheck(&volume, free, value.limits.reserve_bytes)?;
    let destination = value.destination();
    publication::child_directory(&value.base.join("instances"))?;
    std::fs::create_dir(&destination).map_err(|_| InstallError::DestinationExists)?;
    publication::guard(&destination)?;
    publication::child_directory(&destination.join("software"))?;
    publication::child_directory(&destination.join("control"))?;
    publication::child_directory(&destination.join("game"))?;
    for file in &files {
        job.ready(reporter).await?;
        let target = destination.join(&file.path);
        parent(&target)?;
        let source = root.join(
            file.path
                .strip_prefix("software/")
                .ok_or(InstallError::PathRefused)?,
        );
        let mut input = tokio::fs::File::open(source)
            .await
            .map_err(|_| InstallError::PublishFailed)?;
        let mut output = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(target)
            .await
            .map_err(|_| InstallError::PublishFailed)?;
        tokio::io::copy(&mut input, &mut output)
            .await
            .map_err(|_| InstallError::PublishFailed)?;
        output
            .sync_all()
            .await
            .map_err(|_| InstallError::PublishFailed)?;
    }
    let receipt = ProcessedReceipt {
        schema: 3,
        install_id: value.install_id.clone(),
        plan_hash: value.hash(),
        software: value.software,
        version: value.version.clone(),
        loader_version: recipe.loader_version.clone(),
        files,
        inputs: records,
        java_requirement: value.required_java.clone(),
        launch: value.launch_recipe(),
        completed_at: crate::now()?,
    };
    verify_files(&destination, &receipt.files, value.limits.total_bytes).await?;
    let bytes = serde_json::to_vec(&receipt).map_err(|_| InstallError::ReceiptInvalid)?;
    if bytes.len() > 1024 * 1024 {
        return Err(InstallError::LimitExceeded);
    }
    write(&destination.join("control/receipt.json"), &bytes).await?;
    // Delete only the inventoried files after proven successful execution/publication.
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
    reporter.done(true, None);
    Ok(InstallOutcome::Installed(receipt.into_receipt()?))
}
async fn verify_files(root: &Path, files: &[GeneratedFile], cap: u64) -> Result<()> {
    if files.is_empty() || files.len() > MAX_FILES {
        return Err(InstallError::ReceiptInvalid);
    }
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for file in files {
        paths::destination(&file.path)?;
        if !seen.insert(file.path.to_ascii_lowercase()) {
            return Err(InstallError::ReceiptInvalid);
        }
        total = total
            .checked_add(file.size)
            .ok_or(InstallError::LimitExceeded)?;
        if total > cap {
            return Err(InstallError::LimitExceeded);
        }
        // This is a locally generated digest, never serialized as upstream provenance.
        let pin = ArtifactPin {
            url: String::new(),
            digest: PublishedDigest::new(DigestAlgorithm::Sha256, file.sha256.clone())?,
            size: Some(file.size),
        };
        crate::bundle::rehash(&root.join(&file.path), &pin, file.size, &file.sha256, 1).await?;
    }
    Ok(())
}
pub(crate) async fn verify(value: &InstallPlan, deadline: tokio::time::Instant) -> Result<Receipt> {
    let recipe = plan(value)?;
    let root = value.destination();
    let path = root.join("control/receipt.json");
    publication::plain_file(&path)?;
    if std::fs::metadata(&path)
        .map_err(|_| InstallError::ReceiptInvalid)?
        .len()
        > 1024 * 1024
    {
        return Err(InstallError::LimitExceeded);
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| InstallError::ReceiptInvalid)?;
    let receipt: ProcessedReceipt =
        serde_json::from_slice(&bytes).map_err(|_| InstallError::ReceiptInvalid)?;
    if receipt.plan_hash != value.hash()
        || receipt.install_id != value.install_id
        || receipt.software != value.software
        || receipt.version != value.version
        || receipt.loader_version != recipe.loader_version
        || receipt.launch != value.launch_recipe()
        || receipt.java_requirement != value.required_java
    {
        return Err(InstallError::ReceiptInvalid);
    }
    let originals = recipe
        .artifacts
        .iter()
        .chain(recipe.mappings.iter())
        .collect::<Vec<_>>();
    if originals.len() != receipt.inputs.len() {
        return Err(InstallError::ReceiptInvalid);
    }
    for (original, record) in originals.iter().zip(&receipt.inputs) {
        if record.path != format!("software/{}", original.destination)
            || record.url != original.pin.url
            || record.family != original.family
            || Some(record.published_size) != original.pin.size
            || record.published_digest != original.pin.digest
        {
            return Err(InstallError::ReceiptInvalid);
        }
    }
    tokio::time::timeout_at(
        deadline,
        verify_files(&root, &receipt.files, value.limits.total_bytes),
    )
    .await
    .map_err(|_| InstallError::Deadline)??;
    verify_inputs(&root.join("software"), &receipt.inputs).await?;
    let actual = inventory(&root.join("software"), value.limits.total_bytes)?;
    let names = actual
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .map_err(|_| InstallError::PathRefused)
        })
        .collect::<Result<HashSet<_>>>()?;
    let expected = receipt
        .files
        .iter()
        .map(|f| f.path.clone())
        .collect::<HashSet<_>>();
    if names != expected {
        return Err(InstallError::ReceiptInvalid);
    }
    verify_layout(recipe, &root.join("software"), value.limits.artifact_bytes).await?;
    let expected = recipe::argfile(&recipe::launch(recipe, &root.join("software"))?)?;
    if recipe.legacy.is_some()
        && tokio::fs::read(root.join("software/launch.jar"))
            .await
            .map_err(|_| InstallError::ReceiptInvalid)?
            != legacy::pathing_jar(recipe, &root.join("software"))?
    {
        return Err(InstallError::ArtifactChanged);
    }
    if tokio::fs::read(root.join("software/launch.args"))
        .await
        .map_err(|_| InstallError::ReceiptInvalid)?
        != expected
    {
        return Err(InstallError::ArtifactChanged);
    }
    receipt.into_receipt()
}
pub(crate) fn managed_launch(
    installer: &ServerInstaller,
    value: &InstallPlan,
    receipt: &ProcessedReceipt,
    game: &Path,
) -> Result<ManagedLaunch> {
    let game = installer.validate_managed_game(game)?;
    let args = if value.processed.as_ref().is_some_and(|p| p.legacy.is_some()) {
        recipe::launch(plan(value)?, &value.destination().join("software"))?
    } else {
        vec![format!(
            "@{}",
            value
                .destination()
                .join("software/launch.args")
                .to_str()
                .ok_or(InstallError::PathRefused)?
        )]
    };
    Ok(ManagedLaunch {
        java: value.runtime.path.clone(),
        working_directory: game,
        args,
        required_files: vec![value.destination().join(
            if value.processed.as_ref().is_some_and(|p| p.legacy.is_some()) {
                "software/launch.jar"
            } else {
                "software/launch.args"
            },
        )],
        artifacts: receipt
            .files
            .iter()
            .map(|f| ManagedArtifact {
                path: value.destination().join(&f.path),
                size: f.size,
                sha256: f.sha256.clone(),
            })
            .collect(),
    })
}
