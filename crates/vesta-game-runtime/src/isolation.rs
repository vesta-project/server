//! Trusted local server policy composed with the launcher's shared OS adapters.
//! No transport DTO, shell, second AppContainer/bubblewrap implementation, or
//! automatic installation/privilege changes. Native quotas are not supplied here.
use crate::process::RuntimeError;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};
use vesta_sandbox::{
    PathAccess, RunPlan, SandboxPolicy, SandboxPreset, SandboxedSpawn, WrapperNesting,
};

/// Local-administrator-selected runtime directories, never remote file grants.
#[derive(Clone, Eq, PartialEq)]
pub struct ServerIsolation {
    runtime_directories: Vec<PathBuf>,
    protected: Vec<PathBuf>,
}
impl std::fmt::Debug for ServerIsolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerIsolation")
            .field("runtime_count", &self.runtime_directories.len())
            .finish_non_exhaustive()
    }
}
impl ServerIsolation {
    pub(crate) fn validate(&self, program: &Path, game: &Path) -> Result<(), RuntimeError> {
        self.policy(program, game).map(|_| ())
    }
    pub fn new(
        runtime_directories: Vec<PathBuf>,
        protected: Vec<PathBuf>,
    ) -> Result<Self, RuntimeError> {
        if runtime_directories.is_empty()
            || runtime_directories.len() > 16
            || protected.is_empty()
            || runtime_directories
                .iter()
                .chain(&protected)
                .any(|p| !p.is_absolute())
        {
            return Err(RuntimeError::InvalidLaunch);
        }
        Ok(Self {
            runtime_directories,
            protected,
        })
    }

    fn policy(
        &self,
        program: &Path,
        game: &Path,
    ) -> Result<(PathBuf, SandboxPolicy), RuntimeError> {
        let invalid = |_| RuntimeError::IsolationUnavailable;
        let mut paths = vec![game.to_owned(), program.to_owned()];
        paths.extend(self.runtime_directories.iter().cloned());
        paths.extend(self.protected.iter().cloned());
        // Recheck canonical targets for every spawn, not just config parsing.
        let canonical = vesta_sandbox::canonicalize_allowlist(&paths).map_err(invalid)?;
        let game = &canonical[0];
        let program = &canonical[1];
        let runtimes = &canonical[2..2 + self.runtime_directories.len()];
        let protected = &canonical[2 + self.runtime_directories.len()..];
        if !game.is_dir()
            || !program.is_file()
            || runtimes.iter().any(|p| !p.is_dir())
            || !runtimes.iter().any(|p| program.starts_with(p))
            || runtimes.iter().any(|p| overlaps(game, p))
        {
            return Err(RuntimeError::IsolationUnavailable);
        }
        let mut visible = vec![game.clone()];
        visible.extend(runtimes.iter().cloned());
        for path in vesta_sandbox::headless_system_paths() {
            if path.exists() {
                visible.extend(vesta_sandbox::canonicalize_allowlist(&[path]).map_err(invalid)?);
            }
        }
        // Includes config/launch/TLS/state, even when a protected file is absent.
        if protected
            .iter()
            .any(|p| visible.iter().any(|root| overlaps(p, root)))
        {
            return Err(RuntimeError::IsolationUnavailable);
        }
        let mut filesystem = vec![PathAccess::new(game, true, true, false).loadable()];
        filesystem.extend(
            runtimes
                .iter()
                .map(|p| PathAccess::new(p, true, false, false).loadable()),
        );
        Ok((
            program.clone(),
            SandboxPolicy {
                enabled: true,
                preset: SandboxPreset::Modded,
                filesystem_allowlist: filesystem,
                network_allowed: true,
                mic_allowed: false,
                usb_allowed: false,
                exec_allowlist: vec![program.clone()],
                wrapper_nesting: WrapperNesting::SandboxOutside,
                extra_paths: vec![],
            },
        ))
    }

    pub(crate) fn prepare(
        &self,
        program: &Path,
        args: &[String],
        game: &Path,
    ) -> Result<PreparedCommand, RuntimeError> {
        let (program, policy) = self.policy(program, game)?;
        let run = RunPlan::new(&program, args.to_vec(), game, HashMap::new());
        let (spawn, _) = vesta_sandbox::prepare_headless(&run, &policy)
            .map_err(|_| RuntimeError::IsolationUnavailable)?;
        let SandboxedSpawn::Prepared {
            program: helper,
            mut args,
            env,
            cwd,
            placement,
            cleanup_paths,
            ..
        } = spawn
        else {
            return Err(RuntimeError::IsolationUnavailable);
        };
        let lifetime = IsolationLifetime {
            paths: cleanup_paths,
        };
        // Both shared placements wrap this direct game invocation. The durable
        // Vesta supervisor itself is never inside the restricted child.
        match placement {
            vesta_sandbox::SandboxCommandPlacement::WholeCommand
            | vesta_sandbox::SandboxCommandPlacement::GameAndHooks => {
                args.push(
                    program
                        .to_str()
                        .ok_or(RuntimeError::InvalidLaunch)?
                        .to_owned(),
                );
                args.extend(run.args);
            }
        }
        let mut command = Command::new(helper);
        command.args(args).current_dir(cwd).env_clear().envs(env);
        #[cfg(windows)]
        for name in [
            "SystemRoot",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "LOCALAPPDATA",
            "USERPROFILE",
            "HOMEDRIVE",
            "HOMEPATH",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        Ok(PreparedCommand { command, lifetime })
    }
}

fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

pub(crate) struct PreparedCommand {
    pub command: Command,
    pub lifetime: IsolationLifetime,
}
#[derive(Default)]
pub(crate) struct IsolationLifetime {
    paths: Vec<PathBuf>,
}
impl Drop for IsolationLifetime {
    fn drop(&mut self) {
        // Only exact private paths returned by the shared adapter. Never a host
        // config or caller-selected cleanup root. Hold until child I/O is reaped.
        for path in &self.paths {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[cfg(test)]
mod tests;
