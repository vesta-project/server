//! Shared, reviewed source build. Only the verified final JAR is published.
use super::*;
use crate::ArtifactRecord;
use crate::{install as publication, progress::ProgressReporter, InstallPlan, ServerInstaller};
use std::{
    collections::BTreeMap,
    process::{Command, Stdio},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) struct BuiltOutput {
    pub received: crate::transfer::Received,
    pub input: ArtifactRecord,
}

const TREE_ENTRIES: usize = 200_000;
fn check_memory(available: u64, heap_mib: u32) -> Result<()> {
    let required = (u64::from(heap_mib) + 512 + 256) * 1024 * 1024;
    if available < required {
        Err(InstallError::BuildMemoryLow)
    } else {
        Ok(())
    }
}

fn tools(plan: &BuildPlan) -> Result<[&RuntimeBinding; 3]> {
    Ok([
        plan.git.as_ref().ok_or(InstallError::RuntimeUnknown)?,
        plan.shell.as_ref().ok_or(InstallError::RuntimeUnknown)?,
        plan.compiler.as_ref().ok_or(InstallError::RuntimeUnknown)?,
    ])
}
fn command(
    tool: &Path,
    java: &Path,
    recipe: &BuildPlan,
    stage: &Path,
    cwd: &Path,
) -> Result<Command> {
    let [git, shell, _] = tools(recipe)?;
    let mut paths = vec![
        java.parent()
            .ok_or(InstallError::RuntimeUnknown)?
            .to_path_buf(),
        git.path
            .parent()
            .ok_or(InstallError::RuntimeUnknown)?
            .to_path_buf(),
        shell
            .path
            .parent()
            .ok_or(InstallError::RuntimeUnknown)?
            .to_path_buf(),
    ];
    // Git's POSIX commands need its own runtime; never carry caller PATH entries.
    #[cfg(windows)]
    {
        let root = shell
            .path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or(InstallError::RuntimeUnknown)?;
        paths.push(root.join("mingw64/bin"));
    }
    #[cfg(unix)]
    paths.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    let mut cmd = Command::new(tool);
    cmd.env_clear();
    let property_path = |path: PathBuf| -> Result<String> {
        let value = path.to_str().ok_or(InstallError::PathRefused)?;
        if value.contains(['"', '\'', '\n', '\r']) {
            return Err(InstallError::PathRefused);
        }
        #[cfg(windows)]
        let value = value.replace('\\', "/");
        Ok(value.to_string())
    };
    // JVM subprocesses do not inherit -Duser.home from their parent. Carry the
    // private home explicitly so Maven cannot read/write the operator's .m2.
    // Maven writes pom.properties with the JVM line separator. Use canonical
    // LF bytes for every child JVM, without changing the server's launch flags.
    // The quoted LF here is a trusted constant, never user input.
    let java_options = format!(
        "-XX:ActiveProcessorCount=1 -Dline.separator=\"\n\" -Dsun.net.client.defaultConnectTimeout=15000 -Dsun.net.client.defaultReadTimeout=30000 -Duser.home=\"{}\" -Djava.io.tmpdir=\"{}\"",
        property_path(stage.join("home"))?,
        property_path(stage.join("tmp"))?
    );
    for key in ["SystemRoot", "WINDIR", "SYSTEMDRIVE"] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.env(
        "PATH",
        std::env::join_paths(paths).map_err(|_| InstallError::PathRefused)?,
    )
    .env(
        "JAVA_HOME",
        java.parent()
            .and_then(Path::parent)
            .ok_or(InstallError::RuntimeUnknown)?,
    )
    .env("HOME", stage.join("home"))
    .env("USERPROFILE", stage.join("home"))
    .env("XDG_CONFIG_HOME", stage.join("home"))
    .env("TMP", stage.join("tmp"))
    .env("TEMP", stage.join("tmp"))
    .env("TMPDIR", stage.join("tmp"))
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_CONFIG_GLOBAL", stage.join("gitconfig"))
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_HTTP_LOW_SPEED_LIMIT", "1024")
    .env("GIT_HTTP_LOW_SPEED_TIME", "30")
    .env("GCM_INTERACTIVE", "never")
    .env("GIT_CONFIG_COUNT", "3")
    .env("GIT_CONFIG_KEY_0", "core.hooksPath")
    .env("GIT_CONFIG_VALUE_0", stage.join("hooks"))
    .env("GIT_CONFIG_KEY_1", "protocol.file.allow")
    // The reviewed Spigot patch script fetches Bukkit/CraftBukkit locally.
    // Only fixed official HTTPS URLs are used for initial source acquisition.
    .env("GIT_CONFIG_VALUE_1", "user")
    .env("GIT_CONFIG_KEY_2", "protocol.ext.allow")
    .env("GIT_CONFIG_VALUE_2", "never")
    .env("GIT_AUTHOR_NAME", "Vesta")
    .env("GIT_AUTHOR_EMAIL", "build@localhost")
    .env("GIT_COMMITTER_NAME", "Vesta")
    .env("GIT_COMMITTER_EMAIL", "build@localhost")
    // Single-threaded Maven, bounded inherited Java heaps. This is not a
    // host memory quota: the compiler and BuildTools overlap while building.
    .env("MAVEN_OPTS", format!("-Xmx{}m -XX:ActiveProcessorCount=1 -Dmaven.wagon.rto=30000 -Dmaven.wagon.http.retryHandler.count=2", recipe.decompiler_heap_mib))
    .env("MAVEN_ARGS", "--batch-mode --no-transfer-progress")
    .env("_JAVA_OPTIONS", java_options)
    // The root has its own explicit 512 MiB argument. Child tools without a
    // heap argument receive the reviewed decompiler budget. Unlike _JAVA_OPTIONS,
    // JAVA_TOOL_OPTIONS permits explicit root/Maven arguments to override it.
    .env("JAVA_TOOL_OPTIONS", format!("-Xmx{}m", recipe.decompiler_heap_mib))
    .env("LC_ALL", "C")
    .env("LANG", "C")
    .current_dir(cwd)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    Ok(cmd)
}

// Used for periodic accounting and a reviewed deletion inventory. Never follow
// links or recurse through an unbounded tree, including on cleanup.
type Inventory = (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>);
fn internal_alias(root: &Path, path: &Path) -> bool {
    if path != root.join("work/work/decompile-latest") {
        return false;
    }
    let Ok(target) = std::fs::read_link(path) else {
        return false;
    };
    let Some(name) = target.to_str() else {
        return false;
    };
    // BuildTools creates this one diagnostic alias. Never traverse it, and do
    // not exempt arbitrary symlinks/reparse points produced by a source tree.
    name.strip_prefix("decompile-").is_some_and(|id| hex(id, 8))
}
fn scan(root: &Path, cap: u64, collect: bool) -> Result<Inventory> {
    publication::guard(root)?;
    let mut todo = vec![(root.to_owned(), 0usize)];
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut aliases = Vec::new();
    let mut bytes = 0u64;
    let mut count = 0usize;
    while let Some((dir, depth)) = todo.pop() {
        publication::guard(&dir)?;
        if depth > 64 {
            return Err(InstallError::LimitExceeded);
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if !collect && error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(InstallError::PathRefused),
        };
        for entry in entries {
            let path = entry.map_err(|_| InstallError::PathRefused)?.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if !collect && error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(InstallError::PathRefused),
            };
            count += 1;
            if publication::redirected(&metadata) {
                if !internal_alias(root, &path) {
                    return Err(InstallError::PathRefused);
                }
                if collect {
                    aliases.push(path);
                }
            } else if metadata.is_dir() {
                todo.push((path.clone(), depth + 1));
                if collect {
                    dirs.push(path);
                }
            } else if metadata.is_file() {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or(InstallError::LimitExceeded)?;
                if collect {
                    files.push(path);
                }
            } else {
                return Err(InstallError::PathRefused);
            }
            if bytes > cap || count > TREE_ENTRIES {
                return Err(InstallError::LimitExceeded);
            }
        }
    }
    Ok((files, dirs, aliases))
}
fn cleanup(root: &Path, cap: u64) -> Result<()> {
    let (files, mut dirs, aliases) = scan(root, cap, true)?;
    for file in &files {
        publication::plain_file(file)?;
        // Git packs can be read-only on Windows. Change only the inventoried
        // private file's flag, after refusing hard links to another tree.
        let opened = std::fs::File::open(file).map_err(|_| InstallError::PathRefused)?;
        crate::execution_workspace::file_identity_with_links(&opened, 1)?;
    }
    for alias in aliases {
        if !internal_alias(root, &alias) {
            return Err(InstallError::PathRefused);
        }
        #[cfg(windows)]
        std::fs::remove_dir(alias).map_err(|_| InstallError::PublishFailed)?;
        #[cfg(not(windows))]
        std::fs::remove_file(alias).map_err(|_| InstallError::PublishFailed)?;
    }
    for file in files {
        publication::plain_file(&file)?;
        #[cfg(windows)]
        {
            let mut permissions = std::fs::metadata(&file)
                .map_err(|_| InstallError::PathRefused)?
                .permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            // Windows-only attribute, never Unix mode bits.
            permissions.set_readonly(false);
            std::fs::set_permissions(&file, permissions)
                .map_err(|_| InstallError::PublishFailed)?;
        }
        std::fs::remove_file(file).map_err(|_| InstallError::PublishFailed)?;
    }
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for dir in dirs {
        publication::guard(&dir)?;
        std::fs::remove_dir(dir).map_err(|_| InstallError::PublishFailed)?;
    }
    publication::guard(root)?;
    std::fs::remove_dir(root).map_err(|_| InstallError::PublishFailed)
}

async fn run(
    mut cmd: Command,
    stage: &Path,
    value: &InstallPlan,
    deadline: tokio::time::Instant,
    reporter: &dyn ProgressReporter,
) -> Result<Vec<u8>> {
    let log = stage.join(format!("command-{}", uuid::Uuid::new_v4().simple()));
    let compiler = cmd.get_program() == value.runtime.path.as_os_str();
    // All invocations get process-family ownership and bounded output, including
    // Git helpers. Pipe draining never depends on the management reader.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut next = tokio::time::Instant::now();
    crate::build_process::run(
        cmd,
        log.clone(),
        deadline,
        reporter,
        crate::build_process::LogPolicy {
            limit: 8 * 1024 * 1024,
            reject_incomplete_decompilation: compiler,
        },
        || {
            if tokio::time::Instant::now() >= next {
                // Wall-clock approval expiry also bounds work after a host
                // suspend, where the monotonic process deadline may advance differently.
                if crate::now()? >= value.expires {
                    return Err(InstallError::Expired);
                }
                scan(stage, value.limits.total_bytes, false)?;
                if publication::disk_space(&value.base)?.1 < value.limits.reserve_bytes {
                    return Err(InstallError::StorageLow);
                }
                next = tokio::time::Instant::now() + Duration::from_secs(5);
            }
            Ok(())
        },
    )
    .await?;
    let output = tokio::fs::read(log.with_extension("stdout.log"))
        .await
        .map_err(|_| InstallError::RuntimeFailed)?;
    Ok(output)
}

fn repositories(recipe: &BuildPlan) -> [(&'static str, &'static str, &str); 4] {
    [
        ("BuildData", "builddata", &recipe.refs.build_data),
        ("Bukkit", "bukkit", &recipe.refs.bukkit),
        ("CraftBukkit", "craftbukkit", &recipe.refs.craft_bukkit),
        ("Spigot", "spigot", &recipe.refs.spigot),
    ]
}
async fn verify_refs(
    stage: &Path,
    work: &Path,
    value: &InstallPlan,
    recipe: &BuildPlan,
    deadline: tokio::time::Instant,
    reporter: &dyn ProgressReporter,
) -> Result<()> {
    for (name, _, expected) in repositories(recipe) {
        let mut cmd = command(
            &tools(recipe)?[0].path,
            &value.runtime.path,
            recipe,
            stage,
            &work.join(name),
        )?;
        cmd.args(["rev-parse", "refs/heads/master"]);
        let actual = run(cmd, stage, value, deadline, reporter).await?;
        if std::str::from_utf8(&actual)
            .map_err(|_| InstallError::ArtifactChanged)?
            .trim()
            != expected
        {
            return Err(InstallError::ArtifactChanged);
        }
    }
    Ok(())
}

pub(crate) async fn build(
    installer: &ServerInstaller,
    value: &InstallPlan,
    output: &Path,
    reporter: &dyn ProgressReporter,
) -> Result<BuiltOutput> {
    let recipe = value
        .source_build
        .as_ref()
        .ok_or(InstallError::InvalidInput)?;
    if value.schema != 4
        || !matches!(value.software, Software::Bukkit | Software::Spigot)
        || value.processed.is_some()
        || value.bundle.is_some()
        || recipe.seconds != BUILD_SECONDS
        || recipe.reference_policy != "warn_on_local_output_difference"
        || recipe.decompiler_heap_mib != if recipe.java_min >= 25 { 1024 } else { 512 }
    {
        return Err(InstallError::InvalidInput);
    }
    let _heavy = crate::build_process::heavy_slot()?;
    let mut memory = sysinfo::System::new();
    memory.refresh_memory();
    let available = memory
        .cgroup_limits()
        .map_or(memory.available_memory(), |limits| {
            limits.free_memory.min(memory.available_memory())
        });
    check_memory(available, recipe.decompiler_heap_mib)?;
    for tool in tools(recipe)? {
        tool.recheck()?;
    }
    if !value.runtime_compatible() {
        return Err(InstallError::RuntimeIncompatible);
    }
    let stages = value.base.join(".staging");
    publication::child_directory(&stages)?;
    let stage = stages.join(format!("buildtools_{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&stage).map_err(|_| InstallError::PublishFailed)?;
    // Failed jobs are retained for explicit recovery. No recursive deletion in Drop.
    for dir in ["work", "tmp", "home", "hooks"] {
        publication::child_directory(&stage.join(dir))?;
    }
    let work = stage.join("work");
    tokio::fs::write(stage.join("gitconfig"), b"[user]\n name = Vesta\n email = build@localhost\n[core]\n autocrlf = false\n[commit]\n gpgsign = false\n").await.map_err(|_| InstallError::PublishFailed)?;
    let mut network = Job::new(value.limits, value.transferred);
    let tool_path = work.join("BuildTools.jar");
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tool_path)
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    let tool = installer
        .http
        .family_artifact(
            &recipe.tool,
            SourceFamily::SpigotBuildTools,
            &mut network,
            reporter,
            &mut file,
        )
        .await?;
    file.sync_all()
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    drop(file);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(recipe.seconds);
    for (name, remote, reference) in repositories(recipe) {
        reporter.start_step("Preparing server sources", None);
        let path = work.join(name);
        publication::child_directory(&path)?;
        for args in [
            vec!["init".to_owned(), "--initial-branch=master".into()],
            // Normalize generated source commits as BuildTools expects, while
            // retaining LF checkout bytes for reproducible Maven resources.
            vec![
                "config".into(),
                "core.autocrlf".into(),
                if cfg!(windows) { "input" } else { "false" }.into(),
            ],
            vec![
                "remote".into(),
                "add".into(),
                "origin".into(),
                format!("https://hub.spigotmc.org/stash/scm/spigot/{remote}.git"),
            ],
            vec![
                "fetch".into(),
                "--no-tags".into(),
                "origin".into(),
                reference.into(),
            ],
            vec![
                "checkout".into(),
                "-B".into(),
                "master".into(),
                reference.into(),
            ],
        ] {
            let mut cmd = command(
                &tools(recipe)?[0].path,
                &value.runtime.path,
                recipe,
                &stage,
                &path,
            )?;
            cmd.args(args);
            run(cmd, &stage, value, deadline, reporter).await?;
        }
    }
    verify_refs(&stage, &work, value, recipe, deadline, reporter).await?;
    if value.software == Software::Spigot {
        // Upstream otherwise uses local `git clone`, which hard-links objects.
        // Empty owned repos let its patch script fetch the same local branches
        // without creating shared writable links between the source trees.
        for name in ["Spigot-API", "Spigot-Server"] {
            let path = work.join("Spigot").join(name);
            publication::child_directory(&path)?;
            let mut cmd = command(
                &tools(recipe)?[0].path,
                &value.runtime.path,
                recipe,
                &stage,
                &path,
            )?;
            cmd.args(["init", "--initial-branch=master"]);
            run(cmd, &stage, value, deadline, reporter).await?;
            let mut cmd = command(
                &tools(recipe)?[0].path,
                &value.runtime.path,
                recipe,
                &stage,
                &path,
            )?;
            cmd.args([
                "config",
                "core.autocrlf",
                if cfg!(windows) { "input" } else { "false" },
            ]);
            run(cmd, &stage, value, deadline, reporter).await?;
        }
    }
    let info_path = work.join("BuildData/info.json");
    publication::plain_file(&info_path)?;
    let file = tokio::fs::File::open(info_path)
        .await
        .map_err(|_| InstallError::ArtifactChanged)?;
    let mut bytes = Vec::new();
    file.take(65_537)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| InstallError::ArtifactChanged)?;
    if bytes.len() > 65_536 {
        return Err(InstallError::LimitExceeded);
    }
    let info: BTreeMap<String, serde_json::Value> = parse(&bytes)?;
    let minecraft = info
        .get("minecraftVersion")
        .and_then(serde_json::Value::as_str)
        .ok_or(InstallError::InvalidMetadata)?;
    if minecraft.strip_suffix("_unobfuscated").unwrap_or(minecraft) != value.version {
        return Err(InstallError::ArtifactChanged);
    }
    for tool in tools(recipe)? {
        tool.recheck()?;
    }
    value.runtime.recheck()?;
    let mut cmd = command(
        &value.runtime.path,
        &value.runtime.path,
        recipe,
        &stage,
        &work,
    )?;
    let target = match value.software {
        Software::Bukkit => "craftbukkit",
        Software::Spigot => "spigot",
        _ => return Err(InstallError::InvalidInput),
    };
    cmd.args([
        "-Xmx512m",
        "-XX:ActiveProcessorCount=1",
        "-Djava.awt.headless=true",
    ])
    .arg(format!(
        "-Dbt.name={}",
        value.build.ok_or(InstallError::InvalidInput)?
    ))
    .arg(format!("-Duser.home={}", stage.join("home").display()))
    .arg(format!("-Djava.io.tmpdir={}", stage.join("tmp").display()))
    .args([
        "-jar",
        "BuildTools.jar",
        "--nogui",
        "--dont-update",
        "--compile",
        target,
    ]);
    reporter.start_step("Compiling server software", None);
    run(cmd, &stage, value, deadline, reporter).await?;
    verify_refs(&stage, &work, value, recipe, deadline, reporter).await?;
    let jar = work.join(format!("{target}-{}.jar", value.version));
    let (size, hash) = crate::providers::forge::digest(&jar, value.limits.artifact_bytes).await?;
    super::layout::verify(
        &jar,
        value.software,
        &value.version,
        value.limits.artifact_bytes,
    )?;
    let source = tokio::fs::File::open(&jar)
        .await
        .map_err(|_| InstallError::ArtifactChanged)?;
    let mut destination = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    let mut source = source.take(size.checked_add(1).ok_or(InstallError::LimitExceeded)?);
    let copied = crate::transfer::controlled(
        async {
            tokio::io::copy(&mut source, &mut destination)
                .await
                .map_err(|_| InstallError::PublishFailed)
        },
        deadline,
        reporter,
    )
    .await?;
    if copied != size {
        return Err(InstallError::ArtifactChanged);
    }
    destination
        .flush()
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    destination
        .sync_all()
        .await
        .map_err(|_| InstallError::PublishFailed)?;
    drop(destination);
    drop(source);
    let (copied, copied_hash) =
        crate::providers::forge::digest(output, value.limits.artifact_bytes).await?;
    if copied != size || copied_hash != hash {
        return Err(InstallError::ArtifactChanged);
    }
    // Only an inventoried owned tree is removed, after every child has exited.
    // Cleanup failure retains diagnostics; it must not discard a verified result.
    let _ = cleanup(&stage, value.limits.total_bytes);
    Ok(BuiltOutput {
        input: ArtifactRecord {
            path: "build/BuildTools.jar".into(),
            url: recipe.tool.url.clone(),
            family: SourceFamily::SpigotBuildTools,
            published_digest: recipe.tool.digest.clone(),
            published_size: TOOL_BYTES,
            actual_size: tool.size,
            observed_sha256: tool.observed_sha256,
        },
        received: crate::transfer::Received {
            size,
            observed_sha256: hash,
            data: Vec::new(),
            fresh_for: Duration::ZERO,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn low_end_headroom_preserves_the_host_instead_of_starting_an_underfunded_build() {
        let mib = 1024 * 1024;
        // A lean 2 GiB host with 512 MiB already occupied can build the older
        // source profile, but must refuse the larger current decompiler profile.
        assert!(check_memory(1536 * mib, 512).is_ok());
        assert_eq!(
            check_memory(1536 * mib, 1024),
            Err(InstallError::BuildMemoryLow)
        );
        assert!(check_memory(2560 * mib, 1024).is_ok());
        assert_eq!(
            check_memory(512 * mib, 512),
            Err(InstallError::BuildMemoryLow)
        );
    }
    #[test]
    #[ignore = "requires VESTA_BUILDTOOLS_TEST_JAR selecting a completed scratch build; read-only"]
    fn inspect_selected_real_bundle() {
        let path = std::env::var_os("VESTA_BUILDTOOLS_TEST_JAR").expect("selected scratch JAR");
        let software = if std::env::var("VESTA_BUILDTOOLS_TEST_SOFTWARE").unwrap() == "bukkit" {
            Software::Bukkit
        } else {
            Software::Spigot
        };
        super::super::layout::verify(
            Path::new(&path),
            software,
            &std::env::var("VESTA_BUILDTOOLS_TEST_VERSION").unwrap(),
            1024 * 1024 * 1024,
        )
        .unwrap();
    }
    #[test]
    fn source_inventory_limits_disk_and_refuses_linked_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("owned");
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("file"), b"fixture").unwrap();
        assert!(matches!(
            scan(&work, 2, false),
            Err(InstallError::LimitExceeded)
        ));
        let outside = root.path().join("unrelated");
        std::fs::write(&outside, b"keep").unwrap();
        std::fs::hard_link(&outside, work.join("linked")).unwrap();
        assert!(cleanup(&work, 100).is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"keep");
        assert!(work.join("file").exists());
    }
    #[test]
    fn successful_cleanup_removes_only_the_owned_inventory() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("owned");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(work.join("nested")).unwrap();
        std::fs::write(work.join("nested/data"), b"fixture").unwrap();
        cleanup(&work, 100).unwrap();
        assert!(!work.exists());
        assert!(root.path().exists());
    }
    #[cfg(unix)]
    #[test]
    fn only_the_expected_internal_alias_is_allowed_and_never_traversed() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work/work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir(work.join("decompile-1234abcd")).unwrap();
        let alias = work.join("decompile-latest");
        std::os::unix::fs::symlink("decompile-1234abcd", &alias).unwrap();
        assert!(scan(root.path(), 100, false).is_ok());
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink("../../../outside", &alias).unwrap();
        assert!(matches!(
            scan(root.path(), 100, false),
            Err(InstallError::PathRefused)
        ));
    }
}
