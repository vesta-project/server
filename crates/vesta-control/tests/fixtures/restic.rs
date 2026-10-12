//! Inert restic stand-in for backup tests only: a plain directory "repository" with
//! restic's JSON shapes. No encryption, no real restic, never used outside tests.
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::exit,
    time::{SystemTime, UNIX_EPOCH},
};

fn repo() -> PathBuf {
    PathBuf::from(env::var_os("RESTIC_REPOSITORY").expect("repository"))
}

fn require_ready() {
    let password = env::var_os("RESTIC_PASSWORD_FILE").expect("password file");
    if fs::read(password).map_or(true, |p| p.is_empty()) || !repo().join("config").is_file() {
        exit(1);
    }
}

fn copy_tree(from: &Path, to: &Path, skip: &dyn Fn(&str) -> bool, relative: &str) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        if skip(&rel) {
            continue;
        }
        let meta = entry.path().symlink_metadata().unwrap();
        if meta.is_dir() {
            copy_tree(&entry.path(), &to.join(&name), skip, &rel);
        } else if meta.is_file() {
            fs::copy(entry.path(), to.join(&name)).unwrap();
        }
    }
}

fn snapshots_with(tag: &str) -> Vec<(String, u128, Vec<String>)> {
    let mut list = Vec::new();
    let Ok(entries) = fs::read_dir(repo().join("snapshots")) else {
        return list;
    };
    for entry in entries.flatten() {
        let meta = fs::read_to_string(entry.path().join("meta")).unwrap();
        let mut lines = meta.lines();
        let time: u128 = lines.next().unwrap().parse().unwrap();
        let tags: Vec<String> = lines.map(str::to_owned).collect();
        if tags.iter().any(|t| t == tag) {
            list.push((entry.file_name().to_string_lossy().into_owned(), time, tags));
        }
    }
    list.sort_by_key(|s| s.1);
    list
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    assert_eq!(args.first().map(String::as_str), Some("--json"));
    let rest = &args[1..];
    match rest.first().map(String::as_str) {
        Some("init") => {
            if repo().join("config").exists() {
                exit(1);
            }
            fs::create_dir_all(repo().join("snapshots")).unwrap();
            fs::write(repo().join("config"), b"fake").unwrap();
        }
        Some("backup") => {
            require_ready();
            if repo().join("FAIL_BACKUP").exists() {
                exit(1);
            }
            let mut tags = Vec::new();
            let mut excludes = Vec::new();
            let mut i = 2;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--tag" => tags.push(rest[i + 1].clone()),
                    "--exclude-file" => {
                        excludes = fs::read_to_string(&rest[i + 1])
                            .unwrap()
                            .lines()
                            .map(|l| l.trim().to_owned())
                            .filter(|l| !l.is_empty() && !l.starts_with('#'))
                            .collect()
                    }
                    _ => {}
                }
                i += 2;
            }
            let cwd = env::current_dir().unwrap();
            // Simulate an independent writer touching world data mid-scan.
            if cwd.join(".fake-mutate").exists() {
                fs::write(cwd.join("world/region.mca"), b"changed during backup").unwrap();
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let id = format!("{:016x}", now as u64);
            let skip = |rel: &str| {
                excludes.iter().any(|p| {
                    p == rel
                        || p.strip_prefix('*').is_some_and(|ext| rel.ends_with(ext))
                        || p.strip_suffix('/')
                            .is_some_and(|dir| rel == dir || rel.starts_with(&format!("{dir}/")))
                })
            };
            let target = repo().join("snapshots").join(&id);
            copy_tree(&cwd, &target.join("data"), &skip, "");
            fs::write(target.join("meta"), format!("{now}\n{}", tags.join("\n"))).unwrap();
            println!("{{\"message_type\":\"status\",\"percent_done\":1}}");
            println!(
                "{{\"message_type\":\"summary\",\"snapshot_id\":\"{id}\",\"files_new\":1,\"files_changed\":0,\"files_unmodified\":0,\"data_added\":10,\"total_bytes_processed\":10}}"
            );
        }
        Some("snapshots") => {
            require_ready();
            let tag = &rest[2];
            let items: Vec<String> = snapshots_with(tag)
                .into_iter()
                .map(|(id, _, tags)| {
                    let tags: Vec<String> = tags.iter().map(|t| format!("\"{t}\"")).collect();
                    format!(
                        "{{\"id\":\"{id}\",\"time\":\"2026-10-12T00:00:00Z\",\"tags\":[{}]}}",
                        tags.join(",")
                    )
                })
                .collect();
            println!("[{}]", items.join(","));
        }
        Some("forget") => {
            require_ready();
            let tag = &rest[3];
            let keep = rest
                .iter()
                .position(|a| a == "--keep-last")
                .map_or(usize::MAX, |i| rest[i + 1].parse().unwrap());
            let list = snapshots_with(tag);
            let excess = list.len().saturating_sub(keep);
            for (id, ..) in list.into_iter().take(excess) {
                fs::remove_dir_all(repo().join("snapshots").join(id)).unwrap();
            }
        }
        Some("check") => require_ready(),
        Some("tag") => {
            require_ready();
            let id = rest.last().unwrap();
            let meta = repo().join("snapshots").join(id).join("meta");
            let mut text = fs::read_to_string(&meta).unwrap();
            for pair in rest[1..rest.len() - 1].chunks(2) {
                assert_eq!(pair[0], "--add");
                text.push('\n');
                text.push_str(&pair[1]);
            }
            fs::write(meta, text).unwrap();
        }
        Some("restore") => {
            require_ready();
            let id = &rest[1];
            let target = PathBuf::from(&rest[3]);
            let includes: Vec<String> = rest
                .windows(2)
                .filter(|w| w[0] == "--include")
                .map(|w| w[1].trim_start_matches('/').to_owned())
                .collect();
            let source = repo().join("snapshots").join(id).join("data");
            if !source.is_dir() {
                exit(1);
            }
            let skip = |rel: &str| {
                !includes.is_empty()
                    && !includes.iter().any(|inc| {
                        rel == inc
                            || rel.starts_with(&format!("{inc}/"))
                            || inc.starts_with(&format!("{rel}/"))
                    })
            };
            copy_tree(&source, &target, &skip, "");
        }
        _ => exit(1),
    }
}
