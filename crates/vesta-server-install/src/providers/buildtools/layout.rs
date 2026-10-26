//! Bounded validation of the upstream bootstrap packaging contract.
use super::*;

pub(super) fn verify(path: &Path, software: Software, minecraft: &str, cap: u64) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| InstallError::ArtifactChanged)?;
    let mut jar = zip::ZipArchive::new(file).map_err(|_| InstallError::ArtifactChanged)?;
    if jar.is_empty() || jar.len() > 4096 {
        return Err(InstallError::LimitExceeded);
    }
    let mut names = HashSet::new();
    for index in 0..jar.len() {
        let entry = jar
            .by_index(index)
            .map_err(|_| InstallError::ArtifactChanged)?;
        if !names.insert(entry.name().to_owned()) {
            return Err(InstallError::ArtifactChanged);
        }
    }
    let read = |jar: &mut zip::ZipArchive<std::fs::File>, name: &str| -> Result<String> {
        let entry = jar
            .by_name(name)
            .map_err(|_| InstallError::ArtifactChanged)?;
        let mut text = String::new();
        entry
            .take(65_537)
            .read_to_string(&mut text)
            .map_err(|_| InstallError::ArtifactChanged)?;
        if text.len() > 65_536 {
            return Err(InstallError::LimitExceeded);
        }
        Ok(text)
    };
    let manifest = read(&mut jar, "META-INF/MANIFEST.MF")?;
    if !manifest
        .lines()
        .any(|line| line.trim() == "Main-Class: org.bukkit.craftbukkit.bootstrap.Main")
        || !names.contains("org/bukkit/craftbukkit/bootstrap/Main.class")
    {
        return Err(InstallError::ArtifactChanged);
    }
    let target = if software == Software::Bukkit {
        "craftbukkit"
    } else {
        "spigot"
    };
    let mut bytes = 0u64;
    for (list, directory) in [
        ("META-INF/versions.list", "versions"),
        ("META-INF/libraries.list", "libraries"),
    ] {
        let text = read(&mut jar, list)?;
        let lines: Vec<_> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if lines.is_empty() || lines.len() > 128 || directory == "versions" && lines.len() != 1 {
            return Err(InstallError::ArtifactChanged);
        }
        let mut seen = HashSet::new();
        for line in lines {
            let (hash, name) = line.split_once(" *").ok_or(InstallError::ArtifactChanged)?;
            if !hex(hash, 64) || !name.ends_with(".jar") || name.contains('/') || !seen.insert(name)
            {
                return Err(InstallError::ArtifactChanged);
            }
            crate::paths::relative(name, |_, _| true)?;
            if directory == "versions" && !name.starts_with(&format!("{target}-{minecraft}-")) {
                return Err(InstallError::ArtifactChanged);
            }
            // CraftBukkit shades this dependency into the version JAR. Its
            // reviewed bootstrap explicitly skips this checksum-list entry,
            // and bootstrap.xml excludes it from META-INF/libraries.
            if directory == "libraries"
                && name.starts_with(&format!("minecraft-server-{minecraft}-"))
            {
                continue;
            }
            let mut entry = jar
                .by_name(&format!("META-INF/{directory}/{name}"))
                .map_err(|_| InstallError::ArtifactChanged)?;
            let mut digest = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                let n = entry
                    .read(&mut buffer)
                    .map_err(|_| InstallError::ArtifactChanged)?;
                if n == 0 {
                    break;
                }
                bytes = bytes
                    .checked_add(n as u64)
                    .ok_or(InstallError::LimitExceeded)?;
                if bytes > cap {
                    return Err(InstallError::LimitExceeded);
                }
                digest.update(&buffer[..n]);
            }
            if format!("{:x}", digest.finalize()) != hash {
                return Err(InstallError::ArtifactChanged);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    fn fixture(software: Software, mode: &str) -> Vec<u8> {
        let name = if software == Software::Bukkit {
            "craftbukkit"
        } else {
            "spigot"
        };
        let version = format!("{name}-1.21.8-R0.1-SNAPSHOT.jar");
        let runtime = b"inert embedded runtime fixture";
        let library = b"inert embedded dependency fixture";
        let digest = if mode == "hash" {
            "0".repeat(64)
        } else {
            sha256(library)
        };
        let library_name = if mode == "path" {
            "../outside.jar"
        } else {
            "library.jar"
        };
        let manifest = if mode == "main" {
            "Main-Class: wrong.Main\n"
        } else {
            "Manifest-Version: 1.0\nMain-Class: org.bukkit.craftbukkit.bootstrap.Main\n"
        };
        let mut entries = vec![
            ("META-INF/MANIFEST.MF".into(), manifest.as_bytes().to_vec()),
            (
                "org/bukkit/craftbukkit/bootstrap/Main.class".into(),
                b"inert bootstrap fixture".to_vec(),
            ),
            (
                "META-INF/versions.list".into(),
                format!("{} *{version}\n", sha256(runtime)).into_bytes(),
            ),
            (format!("META-INF/versions/{version}"), runtime.to_vec()),
            (
                "META-INF/libraries.list".into(),
                format!(
                    "{digest} *{library_name}\n{} *minecraft-server-1.21.8-R0.1-SNAPSHOT.jar\n",
                    "0".repeat(64)
                )
                .into_bytes(),
            ),
        ];
        if mode != "missing" {
            entries.push(("META-INF/libraries/library.jar".into(), library.to_vec()));
        }
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, data) in entries {
            writer
                .start_file(
                    path,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(&data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    #[test]
    fn bootstrap_identity_checksums_and_shaded_omission_are_checked_together() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("output.jar");
        for software in [Software::Bukkit, Software::Spigot] {
            std::fs::write(&path, fixture(software, "valid")).unwrap();
            verify(&path, software, "1.21.8", 4096).unwrap();
            assert!(verify(&path, software, "1.21.7", 4096).is_err());
            assert!(verify(&path, software, "1.21.8", 2).is_err());
            for mode in ["main", "hash", "path", "missing"] {
                std::fs::write(&path, fixture(software, mode)).unwrap();
                assert!(verify(&path, software, "1.21.8", 4096).is_err(), "{mode}");
            }
        }
    }
}
