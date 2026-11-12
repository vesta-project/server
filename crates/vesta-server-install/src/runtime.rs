use super::{execution, model::*};
use crate::progress::ProgressReporter;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RuntimeBinding {
    pub path: PathBuf,
    pub major: u32,
    pub bytes: u64,
    pub modified_ns: u128,
}
impl RuntimeBinding {
    pub fn snapshot(path: &Path, major: u32) -> Result<Self> {
        super::install::guard(path)?;
        let path = dunce::canonicalize(path).map_err(|_| InstallError::RuntimeFailed)?;
        super::install::plain_file(&path)?;
        if path
            .to_str()
            .is_none_or(|s| s.chars().any(char::is_control))
        {
            return Err(InstallError::RuntimeFailed);
        }
        let m = std::fs::metadata(&path).map_err(|_| InstallError::RuntimeFailed)?;
        let modified_ns = m
            .modified()
            .map_err(|_| InstallError::RuntimeFailed)?
            .duration_since(UNIX_EPOCH)
            .map_err(|_| InstallError::RuntimeFailed)?
            .as_nanos();
        Ok(Self {
            path,
            major,
            bytes: m.len(),
            modified_ns,
        })
    }
    pub fn recheck(&self) -> Result<()> {
        if Self::snapshot(&self.path, self.major)? != *self {
            return Err(InstallError::ApprovalMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) async fn probe(path: &Path, reporter: &dyn ProgressReporter) -> Result<RuntimeBinding> {
    probe_until(
        path,
        reporter,
        tokio::time::Instant::now() + Duration::from_secs(10),
    )
    .await
}

pub(crate) async fn probe_until(
    path: &Path,
    reporter: &dyn ProgressReporter,
    deadline: tokio::time::Instant,
) -> Result<RuntimeBinding> {
    if !path.is_absolute() {
        return Err(InstallError::RuntimeFailed);
    }
    let before = RuntimeBinding::snapshot(path, 0)?;
    let mut budget = execution::Budget::new(Duration::from_secs(10), 16 * 1024)?;
    budget.deadline = budget.deadline.min(deadline);
    let major = run_probe(
        execution::Invocation::probe(&before.path)?,
        budget,
        reporter,
    )
    .await?;
    let after = RuntimeBinding::snapshot(path, 0)?;
    if before != after {
        return Err(InstallError::RuntimeFailed);
    }
    Ok(RuntimeBinding { major, ..after })
}

#[cfg(test)]
pub(crate) async fn probe_command(
    path: &Path,
    args: &[&str],
    reporter: &dyn ProgressReporter,
    duration: Duration,
    cap: usize,
) -> Result<u32> {
    if reporter.is_cancelled() {
        return Err(InstallError::Cancelled);
    }
    if reporter.is_dry_run() {
        return Err(InstallError::RuntimeUnknown);
    }
    if duration > Duration::from_secs(10) || cap > 16 * 1024 {
        return Err(InstallError::InvalidInput);
    }
    let budget = execution::Budget::new(duration, cap)?;
    #[cfg(test)]
    let invocation = if args == ["-version"] {
        execution::Invocation::probe(path)?
    } else {
        execution::Invocation::fixture(
            path,
            args.iter().map(|s| s.to_string()).collect(),
            None,
            execution::Pool::shared(),
        )?
    };
    #[cfg(not(test))]
    let invocation = {
        if args != ["-version"] {
            return Err(InstallError::InvalidInput);
        }
        execution::Invocation::probe(path)?
    };
    run_probe(invocation, budget, reporter).await
}

async fn run_probe(
    invocation: execution::Invocation,
    budget: execution::Budget,
    reporter: &dyn ProgressReporter,
) -> Result<u32> {
    let outcome = execution::run(invocation, budget, reporter).await;
    let output = outcome.result?;
    if outcome.disposition != execution::Disposition::Exited || output.workspace.is_some() {
        return Err(InstallError::RuntimeFailed);
    }
    parse_output(&output.stdout, &output.stderr)
}

fn parse_java_major(version: &str) -> Option<u32> {
    let version = version.strip_prefix("1.").unwrap_or(version);
    version
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse::<u32>()
        .ok()
}

pub(crate) fn parse_output(out: &[u8], err: &[u8]) -> Result<u32> {
    let mut found = None;
    for bytes in [err, out] {
        let text = std::str::from_utf8(bytes).map_err(|_| InstallError::RuntimeFailed)?;
        for line in text.lines() {
            let line = line.trim();
            let version = ["openjdk version \"", "java version \""]
                .iter()
                .find_map(|p| line.strip_prefix(p));
            if let Some(version) = version {
                let token = version
                    .split_once('"')
                    .ok_or(InstallError::RuntimeFailed)?
                    .0;
                let major = parse_java_major(token)
                    .filter(|n| (8..=99).contains(n))
                    .ok_or(InstallError::RuntimeFailed)?;
                if found.is_some_and(|n| n != major) {
                    return Err(InstallError::RuntimeFailed);
                }
                found = Some(major);
            }
        }
    }
    found.ok_or(InstallError::RuntimeFailed)
}
