use super::{GracefulStop, RuntimeError, forbidden_char};
use std::{
    fmt,
    path::PathBuf,
    process::{Command, Stdio},
};

pub const MAX_ARGS: usize = 256;
pub const MAX_ARG_BYTES: usize = 8_192;
pub const MAX_ARGV_BYTES: usize = 32_768;
pub const MAX_PATH_BYTES: usize = 4_096;
pub(crate) const MAX_REQUIRED_FILES: usize = 256;
// Windows includes the executable, quoting, separators and terminating NUL.
const MAX_NATIVE_UNITS: usize = 32_767;

/// Literal trusted executable/argv/cwd, not transport-deserializable launch policy.
///
/// Does not parse JVM options/argfiles, discover Java or verify artifacts/provenance.
/// A future trusted composition must preflight its server-derived software plan.
pub struct LaunchSpec {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    stop: GracefulStop,
    required_files: Vec<PathBuf>,
    isolation: Option<crate::isolation::ServerIsolation>,
}
impl fmt::Debug for LaunchSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaunchSpec")
            .field("argument_count", &self.args.len())
            .field("graceful_stop", &self.stop)
            .finish_non_exhaustive()
    }
}
impl LaunchSpec {
    pub fn new(
        program: PathBuf,
        args: Vec<String>,
        cwd: PathBuf,
        stop: GracefulStop,
    ) -> Result<Self, RuntimeError> {
        let spec = Self {
            program,
            args,
            cwd,
            stop,
            required_files: Vec::new(),
            isolation: None,
        };
        spec.validate()?;
        spec.preflight()?;
        Ok(spec)
    }

    /// Read-only literal identity for trusted host composition (e.g. matching an
    /// installation receipt). These observations confer no process/launch authority.
    pub fn literal_program(&self) -> &std::path::Path {
        &self.program
    }
    pub fn literal_args(&self) -> &[String] {
        &self.args
    }
    pub fn working_directory(&self) -> &std::path::Path {
        &self.cwd
    }
    pub fn graceful_stop(&self) -> GracefulStop {
        self.stop
    }

    /// Opt-in local host composition only. Paths are checked again when preparing
    /// each spawn; callers cannot supply an already-authorized native command.
    pub fn with_isolation(mut self, policy: crate::isolation::ServerIsolation) -> Result<Self, RuntimeError> {
        policy.validate(&self.program, &self.cwd)?;
        self.isolation = Some(policy);
        Ok(self)
    }

    pub fn is_isolated(&self) -> bool { self.isolation.is_some() }

    /// A stopped software replacement inherits the current OS-admin policy;
    /// its launch document cannot remove or broaden the sandbox. Validate the
    /// new executable/runtime against that same policy before publication.
    pub fn constrain_replacement(&self, replacement:Self) -> Result<Self, RuntimeError> {
        match &self.isolation {
            Some(policy) => replacement.with_isolation(policy.clone()),
            None => Ok(replacement),
        }
    }

    fn validate(&self) -> Result<(), RuntimeError> {
        let program = path_text(&self.program)?;
        path_text(&self.cwd)?;
        validate_args(&self.args)?;
        // Refuse scripts on all platforms to keep the portable contract consistent.
        if self
            .program
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
            })
        {
            return Err(RuntimeError::InvalidLaunch);
        }
        let mut bytes = program.len() + 1;
        let mut native = quoted_units(program) + 1;
        for arg in &self.args {
            bytes = bytes
                .checked_add(arg.len() + 1)
                .ok_or(RuntimeError::InvalidLaunch)?;
            native = native
                .checked_add(quoted_units(arg) + 1)
                .ok_or(RuntimeError::InvalidLaunch)?;
        }
        if bytes > MAX_ARGV_BYTES || native > MAX_NATIVE_UNITS {
            return Err(RuntimeError::InvalidLaunch);
        }
        Ok(())
    }

    pub(crate) fn with_required_files(mut self, files: Vec<PathBuf>) -> Result<Self, RuntimeError> {
        if files.len() > MAX_REQUIRED_FILES {
            return Err(RuntimeError::InvalidLaunch);
        }
        for file in &files {
            path_text(file)?;
        }
        self.required_files = files;
        self.preflight()?;
        Ok(self)
    }

    pub(super) fn preflight(&self) -> Result<(), RuntimeError> {
        // Not native confinement or provenance: paths can change after this check.
        if !self.program.is_file()
            || !self.cwd.is_dir()
            || self.required_files.iter().any(|file| !file.is_file())
        {
            return Err(RuntimeError::InvalidLaunch);
        }
        Ok(())
    }

    pub(super) fn stop(&self) -> GracefulStop {
        self.stop
    }

    pub(super) fn command(&self) -> Result<crate::isolation::PreparedCommand, RuntimeError> {
        let mut prepared = match &self.isolation {
            Some(policy) => policy.prepare(&self.program, &self.args, &self.cwd)?,
            None => crate::isolation::PreparedCommand {
                command: Command::new(&self.program),
                lifetime: Default::default(),
            },
        };
        let command = &mut prepared.command;
        if self.isolation.is_none() { command.args(&self.args).current_dir(&self.cwd); }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        super::tree::ProcessTree::configure(command);
        Ok(prepared)
    }
}

pub(crate) fn validate_args(args: &[String]) -> Result<(), RuntimeError> {
    if args.len() > MAX_ARGS
        || args
            .iter()
            .any(|arg| arg.len() > MAX_ARG_BYTES || arg.chars().any(forbidden_char))
    {
        return Err(RuntimeError::InvalidLaunch);
    }
    Ok(())
}

fn path_text(path: &std::path::Path) -> Result<&str, RuntimeError> {
    let text = path.to_str().ok_or(RuntimeError::InvalidLaunch)?;
    if !path.is_absolute()
        || text.is_empty()
        || text.len() > MAX_PATH_BYTES
        || text.chars().any(forbidden_char)
    {
        return Err(RuntimeError::InvalidLaunch);
    }
    Ok(text)
}

// Conservative always-quoted CRT argv size, including backslash/quote expansion.
fn quoted_units(text: &str) -> usize {
    let mut units = 2;
    let mut slashes = 0;
    for c in text.chars() {
        match c {
            '\\' => {
                units += 1;
                slashes += 1;
            }
            '"' => {
                units += slashes + 2;
                slashes = 0;
            }
            _ => {
                units += c.len_utf16();
                slashes = 0;
            }
        }
    }
    units + slashes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_units_include_quotes_backslash_expansion_and_utf16() {
        assert_eq!(quoted_units(""), 2);
        assert_eq!(quoted_units("abc"), 5);
        assert_eq!(quoted_units("a\\"), 5);
        assert_eq!(quoted_units("\\\""), 6);
        assert_eq!(quoted_units("🙂"), 4);
    }

    #[test]
    fn native_quoted_command_boundary_is_inclusive_and_one_unit_over_refuses() {
        let cwd = tempfile::tempdir().unwrap();
        let program = std::env::current_exe().unwrap();
        let base = quoted_units(program.to_str().unwrap()) + 1;
        let mut args = vec!["\\".repeat(4096); 3];
        let remaining = MAX_NATIVE_UNITS - base - 3 * (quoted_units(&args[0]) + 1);
        let units = remaining - 3;
        let final_arg = if units % 2 == 1 { "x" } else { "" }.to_owned() + &"\\".repeat(units / 2);
        args.push(final_arg);
        assert!(
            LaunchSpec::new(
                program.clone(),
                args.clone(),
                cwd.path().into(),
                GracefulStop::Unsupported
            )
            .is_ok()
        );
        args[3].insert(0, 'x');
        assert!(matches!(
            LaunchSpec::new(program, args, cwd.path().into(), GracefulStop::Unsupported),
            Err(RuntimeError::InvalidLaunch)
        ));
    }
}
