use super::*;

pub(super) fn prepare(
    software: Software,
    minecraft: &str,
    coordinate: &str,
    loader_version: String,
    profile: (Vec<Processor>, BTreeMap<String, Sided>),
    embedded: BTreeMap<String, Vec<u8>>,
    artifacts: Vec<BundleArtifact>,
) -> Result<ProcessorPlan> {
    let (processors, data) = profile;
    let data = data
        .into_iter()
        .map(|(key, value)| {
            if key.len() > 64
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                || value.server.len() > 2048
            {
                return Err(InstallError::InvalidMetadata);
            }
            Ok((key, value.server))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut steps = Vec::new();
    for processor in processors {
        if let Some(sides) = &processor.sides {
            if sides
                .iter()
                .any(|s| !matches!(s.as_str(), "server" | "client" | "extract"))
            {
                return Err(InstallError::InvalidMetadata);
            }
            if !sides.iter().any(|s| s == "server") {
                continue;
            }
        }
        if processor.args.len() > 128
            || processor.classpath.len() > 128
            || processor.outputs.len() > 32
            || processor
                .args
                .iter()
                .any(|a| a.len() > 4096 || a.chars().any(char::is_control))
        {
            return Err(InstallError::LimitExceeded);
        }
        for arg in processor.args.iter().filter(|a| a.starts_with('-')) {
            if !matches!(
                arg.as_str(),
                "--task"
                    | "--archive"
                    | "--from"
                    | "--to"
                    | "--exec"
                    | "--optional"
                    | "--input"
                    | "--output"
                    | "--libraries"
                    | "--jar-only"
                    | "--key"
                    | "--version"
                    | "--side"
                    | "--left"
                    | "--right"
                    | "--classes"
                    | "--fields"
                    | "--methods"
                    | "--reverse-right"
                    | "--slim"
                    | "--extra"
                    | "--srg"
                    | "--names"
                    | "--ann-fix"
                    | "--ids-fix"
                    | "--src-fix"
                    | "--record-fix"
                    | "--clean"
                    | "--apply"
                    | "--data"
                    | "--unpatched"
                    | "--store"
                    | "--no-mod-manifest"
                    | "--extract-libraries-to"
                    | "--apply-patches"
            ) {
                return Err(InstallError::Unavailable);
            }
        }
        let main = approved_tool(software, &processor.jar, &processor.args)?;
        let jar = format!("libraries/{}", maven(&processor.jar)?);
        if !artifacts.iter().any(|a| a.destination == jar) {
            return Err(InstallError::InvalidMetadata);
        }
        let classpath = processor
            .classpath
            .iter()
            .map(|c| Ok(format!("libraries/{}", maven(c)?)))
            .collect::<Result<Vec<_>>>()?;
        if classpath
            .iter()
            .any(|p| !artifacts.iter().any(|a| &a.destination == p))
        {
            return Err(InstallError::InvalidMetadata);
        }
        steps.push(Step {
            jar,
            main,
            classpath,
            args: processor.args,
            outputs: processor.outputs,
        });
    }
    if steps.is_empty() {
        return Err(InstallError::Unavailable);
    }
    let argument_file = if cfg!(windows) {
        "data/win_args.txt"
    } else {
        "data/unix_args.txt"
    };
    let mut launch = tokens(
        embedded
            .get(argument_file)
            .ok_or(InstallError::Unavailable)?,
    )?;
    let mut shim_libraries = BTreeMap::new();
    if software == Software::Forge && launch.iter().any(|a| a == "-jar") {
        let name = format!("forge-{coordinate}-shim.jar");
        let shim = format!(
            "libraries/{}",
            maven(&format!("net.minecraftforge:forge:{coordinate}:shim"))?
        );
        if !artifacts.iter().any(|a| a.destination == shim) {
            return Err(InstallError::InvalidMetadata);
        }
        let index = launch
            .iter()
            .position(|a| a == "-jar")
            .ok_or(InstallError::InvalidMetadata)?;
        if launch.get(index + 1) != Some(&name) || index + 2 != launch.len() {
            return Err(InstallError::Unavailable);
        }
        let shim_bytes = embedded
            .get(&format!(
                "maven/{}",
                maven(&format!("net.minecraftforge:forge:{coordinate}:shim"))?
            ))
            .ok_or(InstallError::Unavailable)?;
        let (classpath, hashes) = shim_classpath(shim_bytes)?;
        shim_libraries = hashes;
        launch.truncate(index);
        launch.extend([
            "-cp".into(),
            classpath.join(if cfg!(windows) { ";" } else { ":" }),
            "net.minecraftforge.bootstrap.ForgeBootstrap".into(),
            "--launchTarget".into(),
            "forge_server".into(),
        ]);
        steps.push(Step {
            jar: shim,
            main: String::new(),
            classpath: Vec::new(),
            args: vec!["--onlyCheckJava".into()],
            outputs: BTreeMap::new(),
        });
    }
    // New recipes are deliberate additions, never a generic execute-any-profile switch.
    let plan = ProcessorPlan {
        minecraft: minecraft.into(),
        loader_version,
        coordinate: coordinate.into(),
        artifacts,
        embedded,
        steps,
        data,
        launch,
        mappings: None,
        shim_libraries,
        disk_cap: 0,
    };
    validate_launch(&plan.launch)?;
    Ok(plan)
}

/// The current Forge shim hardcodes a cwd-relative libraries folder. Reproduce
/// its checked classpath/bootstrap arguments with absolute paths for managed cwd.
fn shim_classpath(bytes: &[u8]) -> Result<(Vec<String>, BTreeMap<String, String>)> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| InstallError::InvalidMetadata)?;
    let mut read = |name: &str| -> Result<String> {
        let entry = archive
            .by_name(name)
            .map_err(|_| InstallError::InvalidMetadata)?;
        if entry.size() > 128 * 1024 {
            return Err(InstallError::LimitExceeded);
        }
        let mut value = String::new();
        entry
            .take(128 * 1024 + 1)
            .read_to_string(&mut value)
            .map_err(|_| InstallError::InvalidMetadata)?;
        if value.len() > 128 * 1024 {
            return Err(InstallError::LimitExceeded);
        }
        Ok(value)
    };
    let properties = read("bootstrap-shim.properties")?;
    let properties = properties
        .lines()
        .filter_map(|l| l.split_once('='))
        .collect::<BTreeMap<_, _>>();
    if properties.get("Main-Class").copied() != Some("net.minecraftforge.bootstrap.ForgeBootstrap")
        || properties.get("Arguments").copied() != Some("--launchTarget forge_server")
        || properties.get("Java-Version").copied() != Some("25")
    {
        return Err(InstallError::Unavailable);
    }
    let manifest = read("META-INF/MANIFEST.MF")?
        .replace("\r\n ", "")
        .replace("\n ", "");
    let mut classpath = manifest
        .lines()
        .find_map(|l| l.strip_prefix("Class-Path: "))
        .ok_or(InstallError::InvalidMetadata)?
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let list = read("bootstrap-shim.list")?;
    let mut hashes = BTreeMap::new();
    for line in list.lines().filter(|l| !l.is_empty()) {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(InstallError::InvalidMetadata);
        }
        let digest = PublishedDigest::new(DigestAlgorithm::Sha256, fields[0].into())?;
        let path = format!("libraries/{}", fields[2]);
        relative(&path)?;
        if hashes.insert(path.clone(), digest.hex).is_some() {
            return Err(InstallError::InvalidMetadata);
        }
        if !classpath.contains(&path) {
            classpath.push(path);
        }
        if classpath.len() > MAX_FILES {
            return Err(InstallError::LimitExceeded);
        }
    }
    for path in &classpath {
        relative(path)?;
        if !path.starts_with("libraries/") || !path.ends_with(".jar") {
            return Err(InstallError::InvalidMetadata);
        }
    }
    Ok((classpath, hashes))
}

fn approved_tool(software: Software, coordinate: &str, args: &[String]) -> Result<String> {
    let p: Vec<_> = coordinate.split(':').collect();
    if p.len() < 3 {
        return Err(InstallError::InvalidMetadata);
    }
    let group = p[0];
    let tool = p[1];
    let forge = software == Software::Forge;
    let allowed = if forge {
        group == "net.minecraftforge"
    } else {
        matches!(group, "net.neoforged.installertools" | "net.neoforged")
    };
    if !allowed {
        return Err(InstallError::Unavailable);
    }
    let main = match tool {
        "installertools" => {
            let task = args
                .windows(2)
                .find(|a| a[0] == "--task")
                .map(|a| a[1].as_str())
                .ok_or(InstallError::InvalidMetadata)?;
            if !matches!(
                task,
                "EXTRACT_FILES"
                    | "BUNDLER_EXTRACT"
                    | "MCP_DATA"
                    | "DOWNLOAD_MOJMAPS"
                    | "MERGE_MAPPING"
                    | "PROCESS_MINECRAFT_JAR"
            ) {
                return Err(InstallError::Unavailable);
            }
            if forge {
                "net.minecraftforge.installertools.ConsoleTool"
            } else {
                "net.neoforged.installertools.ConsoleTool"
            }
        }
        "jarsplitter" => {
            if forge {
                "net.minecraftforge.jarsplitter.ConsoleTool"
            } else {
                "net.neoforged.jarsplitter.ConsoleTool"
            }
        }
        "ForgeAutoRenamingTool" if forge => "net.minecraftforge.fart.Main",
        "AutoRenamingTool" if !forge => "net.neoforged.art.Main",
        "binarypatcher" => {
            if forge {
                "net.minecraftforge.binarypatcher.ConsoleTool"
            } else {
                "net.neoforged.binarypatcher.ConsoleTool"
            }
        }
        _ => return Err(InstallError::Unavailable),
    };
    Ok(main.into())
}

fn tokens(bytes: &[u8]) -> Result<Vec<String>> {
    let text = std::str::from_utf8(bytes).map_err(|_| InstallError::InvalidMetadata)?;
    if text.len() > 128 * 1024 || text.contains(['"', '\'', '\\']) {
        return Err(InstallError::Unavailable);
    }
    let result = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .flat_map(str::split_whitespace)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if result.len() > 256 {
        return Err(InstallError::LimitExceeded);
    }
    Ok(result)
}
fn validate_launch(tokens: &[String]) -> Result<()> {
    let shim = tokens.windows(2).any(|t| {
        t[0] == "-jar"
            && t[1].starts_with("libraries/net/minecraftforge/forge/")
            && t[1].ends_with("-shim.jar")
    });
    if !shim
        && !tokens.iter().any(|t| {
            matches!(
                t.as_str(),
                "cpw.mods.bootstraplauncher.BootstrapLauncher"
                    | "net.neoforged.fml.startup.Server"
                    | "net.minecraftforge.bootstrap.ForgeBootstrap"
            )
        })
    {
        return Err(InstallError::Unavailable);
    }
    for token in tokens {
        if token.starts_with('@')
            || token.contains("..")
            || token.chars().any(char::is_control)
            || token.starts_with("-javaagent")
            || token.starts_with("-agent")
            || token.starts_with("-Xbootclasspath")
            || token.starts_with("-XX:")
                && !matches!(
                    token.as_str(),
                    "-XX:+UseCompactObjectHeaders" | "-XX:StackShadowPages=32"
                )
            || token.starts_with("-Djava.") && !token.starts_with("-Djava.net.preferIPv6Addresses=")
        {
            return Err(InstallError::InvalidMetadata);
        }
    }
    Ok(())
}
pub(super) fn launch(plan: &ProcessorPlan, root: &std::path::Path) -> Result<Vec<String>> {
    validate_launch(&plan.launch)?;
    let library = root
        .join("libraries")
        .to_str()
        .ok_or(InstallError::PathRefused)?
        .replace('\\', "/");
    if library.contains(if cfg!(windows) { ';' } else { ':' })
        || library.chars().any(char::is_control)
    {
        return Err(InstallError::PathRefused);
    }
    let mut args = plan
        .launch
        .iter()
        .map(|s| {
            s.replace("libraries/", &(library.clone() + "/")).replace(
                "-DlibraryDirectory=libraries",
                &format!("-DlibraryDirectory={library}"),
            )
        })
        .collect::<Vec<_>>();
    args.push("nogui".into());
    Ok(args)
}
pub(super) fn argfile(args: &[String]) -> Result<Vec<u8>> {
    let mut result = String::new();
    for arg in args {
        if arg.chars().any(char::is_control) {
            return Err(InstallError::InvalidInput);
        }
        result.push('"');
        result.push_str(&arg.replace('\\', "\\\\").replace('"', "\\\""));
        result.push_str("\"\n");
    }
    if result.len() > 128 * 1024 {
        return Err(InstallError::LimitExceeded);
    }
    Ok(result.into_bytes())
}

pub(super) fn resolve(plan: &ProcessorPlan, value: &str, root: &std::path::Path) -> Result<String> {
    let root = root
        .to_str()
        .ok_or(InstallError::PathRefused)?
        .replace('\\', "/");
    let mut values = plan.data.clone();
    values.insert("MINECRAFT_VERSION".into(), plan.minecraft.clone());
    for (key, value) in [
        ("ROOT", root.clone()),
        ("LIBRARY_DIR", format!("{root}/libraries")),
        ("MINECRAFT_JAR", format!("{root}/minecraft.jar")),
        ("INSTALLER", format!("{root}/installer.jar")),
        ("SIDE", "server".into()),
    ] {
        values.insert(key.into(), value);
    }
    fn expand(
        value: &str,
        values: &BTreeMap<String, String>,
        root: &str,
        depth: usize,
    ) -> Result<String> {
        if depth > 16 {
            return Err(InstallError::InvalidMetadata);
        }
        if let Some(coordinate) = value.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            return Ok(format!("{root}/libraries/{}", maven(coordinate)?));
        }
        if let Some(literal) = value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
            return Ok(literal.into());
        }
        let mut value = value.to_owned();
        let mut replacements = 0;
        while let Some(start) = value.find('{') {
            replacements += 1;
            if replacements > 128 {
                return Err(InstallError::InvalidMetadata);
            }
            let end = value[start..]
                .find('}')
                .map(|v| v + start)
                .ok_or(InstallError::InvalidMetadata)?;
            let key = &value[start + 1..end];
            let replacement = expand(
                values.get(key).ok_or(InstallError::InvalidMetadata)?,
                values,
                root,
                depth + 1,
            )?;
            value.replace_range(start..=end, &replacement);
            if value.len() > 16384 {
                return Err(InstallError::LimitExceeded);
            }
        }
        if value.contains('}') {
            return Err(InstallError::InvalidMetadata);
        }
        if value.starts_with("/data/") {
            value = format!("{root}/{}", value.trim_start_matches('/'));
        }
        Ok(value)
    }
    expand(value, &values, &root, 0)
}
