//! Trusted same-account stopped-launch replacement using immutable private artifacts.
use crate::{Result, record};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use vesta_state::{StateDirectory, artifacts::ArtifactDirectory};

pub fn stage_launch(state: &StateDirectory, document: &[u8]) -> Result<String> {
    if document.is_empty() || document.len() > 65_536 {
        return Err("supervisor_launch_invalid");
    }
    let hash = format!("{:x}", Sha256::digest(document));
    let directory = state
        .supervisor()
        .map_err(|_| "supervisor_launch_unavailable")?;
    let name = record::name(&format!("software-{hash}.toml"))?;
    if let Some(mut file) = directory
        .open(&name)
        .map_err(|_| "supervisor_launch_unavailable")?
    {
        let mut previous = Vec::new();
        Read::by_ref(&mut file)
            .take(65_537)
            .read_to_end(&mut previous)
            .map_err(|_| "supervisor_launch_unavailable")?;
        if previous != document {
            return Err("supervisor_launch_invalid");
        }
        return Ok(hash);
    }
    let mut file = directory
        .create_new(&name)
        .map_err(|_| "supervisor_launch_unavailable")?;
    file.write_all(document)
        .and_then(|()| file.sync_all())
        .map_err(|_| "supervisor_launch_unavailable")?;
    drop(file);
    directory
        .sync()
        .map_err(|_| "supervisor_launch_unavailable")?;
    Ok(hash)
}
pub(crate) fn read(directory: &ArtifactDirectory, hash: &str) -> Result<Vec<u8>> {
    if !valid_hash(hash) {
        return Err("supervisor_launch_invalid");
    }
    let file = directory
        .open(&record::name(&format!("software-{hash}.toml"))?)
        .map_err(|_| "supervisor_launch_unavailable")?
        .ok_or("supervisor_launch_unavailable")?;
    let mut bytes = Vec::new();
    file.take(65_537)
        .read_to_end(&mut bytes)
        .map_err(|_| "supervisor_launch_unavailable")?;
    if bytes.is_empty() || bytes.len() > 65_536 || format!("{:x}", Sha256::digest(&bytes)) != hash {
        return Err("supervisor_launch_invalid");
    }
    Ok(bytes)
}
pub(crate) fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
