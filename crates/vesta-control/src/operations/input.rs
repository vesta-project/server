//! Closed, server-owned derivation. There is deliberately no production binding
//! or public registry/injection API yet. Adding a real domain binding needs review;
//! a capability identifier does not implement an action or authenticate a caller.
use super::domain::{
    MAX_INPUT_BYTES, OperationError, OperationErrorCode, OperationsResult, identifier, invalid,
};
use crate::identity::{Capability, Resource};
use sha2::{Digest, Sha256};

#[derive(Clone, Eq, PartialEq)]
pub(super) struct Requirement {
    pub(super) capability: Capability,
    pub(super) resource: Option<Resource>,
}

/// No Debug/Serialize: this contains requested input. Its only constructor is the
/// closed derivation below, never caller-selected safety/target/lock parameters.
pub(super) struct PreparedOperation {
    #[cfg(test)]
    pub(super) operation_type: String,
    pub(super) canonical_input: String,
    pub(super) payload_hash: String,
    pub(super) target: String,
    pub(super) requirements: Vec<Requirement>,
    pub(super) resources: Vec<String>,
    pub(super) effect_idempotent: bool,
    pub(super) requires_confirmation: bool,
}

pub(super) fn digest(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

pub(super) fn derive(operation_type: &str, raw_input: &str) -> OperationsResult<PreparedOperation> {
    if !identifier(operation_type, 64)
        || raw_input.len() > MAX_INPUT_BYTES
        || raw_input.contains('\0')
    {
        return Err(invalid());
    }
    // Only private unit tests register these typed, side-effect-free contracts.
    // Production remains closed rather than advertising a fake executable action.
    #[cfg(test)]
    if matches!(
        operation_type,
        "fixture.write" | "fixture.safe" | "fixture.confirm" | "fixture.unstable"
    ) {
        return fixture::derive(operation_type, raw_input);
    }
    Err(OperationError::rejected(OperationErrorCode::UnknownType))
}

#[cfg(test)]
mod fixture {
    use super::super::domain::{MAX_REQUIREMENTS, MAX_RESOURCES, lock_key, text};
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeSet;

    /// A fixture atomic multi-file value update, not a no-op production handler.
    /// Paths are typed operation targets; callers never supply capability/locks/
    /// confirmation/replay/source declarations. This module executes no effect.
    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        version: u8,
        paths: Vec<String>,
        value: String,
    }
    #[derive(Eq, PartialEq)]
    struct Semantics {
        target: String,
        requirements: Vec<Requirement>,
        resources: Vec<String>,
    }
    fn parse(raw: &str) -> OperationsResult<Input> {
        let parsed: Input = serde_json::from_str(raw).map_err(|_| invalid())?;
        if parsed.version != 1
            || parsed.paths.is_empty()
            || parsed.paths.len() >= MAX_REQUIREMENTS
            || parsed.paths.len() > MAX_RESOURCES
            || !text(&parsed.value, 1_024)
        {
            return Err(invalid());
        }
        let mut seen = BTreeSet::new();
        for path in &parsed.paths {
            if path.is_empty() || path.len() > 128 || !seen.insert(path.as_str()) {
                return Err(invalid());
            }
            Resource::parse(path).map_err(|_| invalid())?;
        }
        Ok(parsed)
    }
    fn semantics(input: &Input) -> OperationsResult<Semantics> {
        let paths: BTreeSet<_> = input.paths.iter().collect();
        let target = format!(
            "files:{}",
            paths
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        if !text(&target, 512) {
            return Err(invalid());
        }
        let mut requirements = vec![Requirement {
            capability: Capability::ConfigEdit,
            resource: None,
        }];
        let mut resources = BTreeSet::new();
        for path in paths {
            requirements.push(Requirement {
                capability: Capability::FilesOverwrite,
                resource: Some(Resource::parse(path).map_err(|_| invalid())?),
            });
            // Case-conservative conflicts; lexical authorization resources remain
            // case-sensitive for allows. No filesystem confinement is claimed.
            let lock = format!("fixture-files:{}", path.to_ascii_lowercase());
            if !lock_key(&lock) {
                return Err(invalid());
            }
            resources.insert(lock);
        }
        Ok(Semantics {
            target,
            requirements,
            resources: resources.into_iter().collect(),
        })
    }
    pub(super) fn derive(kind: &str, raw: &str) -> OperationsResult<PreparedOperation> {
        let mut input = parse(raw)?;
        let original = semantics(&input)?;
        input.paths.sort();
        if kind == "fixture.unstable" {
            // Negative fixture: text stabilization alone must never authorize a
            // canonicalization that changes the capability/target/lock meaning.
            input.paths = vec!["different-target".to_owned()];
        }
        let canonical_input = serde_json::to_string(&input).map_err(|_| invalid())?;
        if canonical_input.len() > MAX_INPUT_BYTES {
            return Err(invalid());
        }
        let reparsed = parse(&canonical_input)?;
        let stable = serde_json::to_string(&reparsed).map_err(|_| invalid())?;
        let bound = semantics(&reparsed)?;
        if stable != canonical_input || bound != original {
            return Err(invalid());
        }
        Ok(PreparedOperation {
            operation_type: kind.to_owned(),
            payload_hash: digest(&canonical_input),
            canonical_input,
            target: bound.target,
            requirements: bound.requirements,
            resources: bound.resources,
            effect_idempotent: kind == "fixture.safe",
            requires_confirmation: kind == "fixture.confirm",
        })
    }
}
