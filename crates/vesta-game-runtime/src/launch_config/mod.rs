//! Bounded trusted local launch definitions, not transport input or authority.
//!
//! Parsing has no filesystem/process effects. Compilation requires a literal
//! executable and preflight; the resulting spec retains artifact checks for the
//! real spawn boundary. No PATH lookup, shell, automatic start or legacy fallback.
use crate::process::{GracefulStop, LaunchSpec, RuntimeError};
use serde::Deserialize;
use std::{
    error::Error,
    fmt,
    path::{Component, Path, PathBuf},
};

pub const MAX_CONFIG_BYTES: usize = 65_536;
pub const MAX_ARGFILES: usize = crate::process::MAX_REQUIRED_FILES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchKind {
    JavaJar,
    JavaArgfile,
    Native,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchConfigError {
    InvalidConfiguration,
    TooLarge,
    JavaUnavailable,
    Preflight(RuntimeError),
}
impl LaunchConfigError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "launch_config_invalid",
            Self::TooLarge => "launch_config_too_large",
            Self::JavaUnavailable => "launch_config_java_unavailable",
            Self::Preflight(error) => error.code(),
        }
    }
}
impl fmt::Display for LaunchConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Launch configuration failed")
    }
}
impl Error for LaunchConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Preflight(error) => Some(error),
            _ => None,
        }
    }
}
impl From<RuntimeError> for LaunchConfigError {
    fn from(error: RuntimeError) -> Self {
        Self::Preflight(error)
    }
}
type Result<T> = std::result::Result<T, LaunchConfigError>;

/// A validated plan with private paths/arguments and redacted Debug. It does not
/// implement Deserialize/Serialize: only the bounded parser constructs it.
/// This is a launch-only document, not the full Java wrapper configuration.
pub struct LaunchConfiguration {
    kind: LaunchKind,
    program: Option<PathBuf>,
    cwd: PathBuf,
    args: Vec<String>,
    required_files: Vec<PathBuf>,
    stop: GracefulStop,
}
impl fmt::Debug for LaunchConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaunchConfiguration")
            .field("kind", &self.kind)
            .field("argument_count", &self.args.len())
            .field("required_file_count", &self.required_files.len())
            .field("graceful_stop", &self.stop)
            .finish_non_exhaustive()
    }
}
impl LaunchConfiguration {
    /// Parse a launch-only TOML document against an explicit absolute host base.
    /// Missing/malformed documents and unknown keys never create defaults.
    /// Paths are resolved lexically, not canonicalized: artifacts may be prepared
    /// later. This neither opens a config file nor supplies native confinement.
    pub fn parse_toml(bytes: &[u8], base: &Path) -> Result<Self> {
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(LaunchConfigError::TooLarge);
        }
        check_absolute(base)?;
        let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
        let document: Document = toml::from_str(text).map_err(|_| invalid())?;
        let cwd = resolve(base, &document.minecraft.working_directory)?;
        let (kind, program, mut args, required_files, stop, trailing) = match document.launch {
            Definition::JavaJar {
                java_command,
                jvm_args,
                jar,
                args,
                stop,
            } => {
                let jar = resolve(&cwd, &jar)?;
                let mut prefix = jvm_args;
                prefix.push("-jar".into());
                prefix.push(path_string(&jar)?.to_owned());
                (
                    LaunchKind::JavaJar,
                    java_command,
                    prefix,
                    vec![jar],
                    stop,
                    args,
                )
            }
            Definition::JavaArgfile {
                java_command,
                jvm_args,
                argfiles,
                args,
                stop,
            } => {
                if argfiles.is_empty() || argfiles.len() > MAX_ARGFILES {
                    return Err(invalid());
                }
                let mut prefix = jvm_args;
                let mut required = Vec::with_capacity(argfiles.len());
                for file in argfiles {
                    let file = resolve(&cwd, &file)?;
                    prefix.push(format!("@{}", path_string(&file)?));
                    required.push(file);
                }
                (
                    LaunchKind::JavaArgfile,
                    java_command,
                    prefix,
                    required,
                    stop,
                    args,
                )
            }
            Definition::Native {
                executable,
                args,
                stop,
                required_files,
                jvm_args,
            } => (
                LaunchKind::Native,
                Some(executable),
                jvm_args,
                required_files
                    .iter()
                    .map(|file| resolve(&cwd, file))
                    .collect::<Result<Vec<_>>>()?,
                stop,
                args,
            ),
        };
        args.extend(trailing);
        crate::process::validate_args(&args).map_err(|_| invalid())?;
        // A default Java path is not known yet, but the argument-only budget is.
        let bytes = args
            .iter()
            .try_fold(0usize, |total, arg| total.checked_add(arg.len() + 1));
        if bytes.is_none_or(|bytes| bytes > crate::process::MAX_ARGV_BYTES) {
            return Err(invalid());
        }
        let program = program.map(|program| resolve(&cwd, &program)).transpose()?;
        Ok(Self {
            kind,
            program,
            cwd,
            args,
            required_files,
            stop: stop.into(),
        })
    }

    /// Resolved working directory; host composition must bind this to its game root
    /// and validate protected-state separation before opening state or runtime ownership.
    pub fn working_directory(&self) -> &Path {
        &self.cwd
    }

    pub fn kind(&self) -> LaunchKind {
        self.kind
    }
    pub fn graceful_stop(&self) -> GracefulStop {
        self.stop
    }

    /// Consume the plan into the existing owned-process contract. If Java was
    /// omitted, trusted host composition must supply its selected absolute path;
    /// no implicit `java`, PATH search, JRE discovery or permission is invented.
    /// Jar/argfile bytes are not parsed, executed or verified as provenance here.
    pub fn into_launch_spec(self, selected_java: Option<&Path>) -> Result<LaunchSpec> {
        let program = match self.program {
            Some(program) => program,
            None => {
                let java = selected_java.ok_or(LaunchConfigError::JavaUnavailable)?;
                check_absolute(java)?;
                java.to_owned()
            }
        };
        Ok(LaunchSpec::new(program, self.args, self.cwd, self.stop)?
            .with_required_files(self.required_files)?)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    minecraft: GameDirectory,
    launch: Definition,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct GameDirectory {
    working_directory: String,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum Definition {
    #[serde(rename_all = "kebab-case")]
    JavaJar {
        java_command: Option<String>,
        #[serde(default)]
        jvm_args: Vec<String>,
        jar: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        stop: Stop,
    },
    #[serde(rename_all = "kebab-case")]
    JavaArgfile {
        java_command: Option<String>,
        #[serde(default)]
        jvm_args: Vec<String>,
        argfiles: Vec<String>,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        stop: Stop,
    },
    Native {
        executable: String,
        #[serde(default, rename = "jvm-args")]
        jvm_args: Vec<String>,
        #[serde(default)]
        args: Vec<String>,
        stop: Stop,
        #[serde(default, rename = "required-files")]
        required_files: Vec<String>,
    },
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Stop {
    #[default]
    ConsoleStop,
    Unsupported,
}
impl From<Stop> for GracefulStop {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::ConsoleStop => Self::MinecraftCommand,
            Stop::Unsupported => Self::Unsupported,
        }
    }
}

fn invalid() -> LaunchConfigError {
    LaunchConfigError::InvalidConfiguration
}
fn path_string(path: &Path) -> Result<&str> {
    let text = path.to_str().ok_or_else(invalid)?;
    if text.trim().is_empty()
        || text.len() > crate::process::MAX_PATH_BYTES
        || text.chars().any(crate::process::forbidden_char)
    {
        return Err(invalid());
    }
    Ok(text)
}
fn check_absolute(path: &Path) -> Result<()> {
    path_string(path)?;
    if !path.is_absolute() {
        return Err(invalid());
    }
    Ok(())
}
fn resolve(base: &Path, text: &str) -> Result<PathBuf> {
    let input = Path::new(text);
    path_string(input)?;
    // Reject Windows drive-relative/root-relative paths instead of silently
    // assigning a drive or current directory. Fully absolute paths are explicit.
    if !input.is_absolute()
        && (input.has_root() || matches!(input.components().next(), Some(Component::Prefix(_))))
    {
        return Err(invalid());
    }
    let joined = if input.is_absolute() {
        input.to_owned()
    } else {
        base.join(input)
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => (),
            Component::ParentDir => {
                if !matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    return Err(invalid());
                }
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    check_absolute(&normalized)?;
    Ok(normalized)
}

#[cfg(test)]
mod tests;
