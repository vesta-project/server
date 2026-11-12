use crate::{ControlError, IdentityErrorCode, Result};
use vesta_state::audit::MAX_METADATA_VALUE_UNITS;

/// A `Resource` only has to name the thing being authorized, so its own bound is generous.
/// A `ResourceScope`, by contrast, is recorded verbatim in the audit record of every grant that
/// carries it, so its bound is the audit metadata value bound and is applied in
/// `ResourceScope::parse` where the scope text actually takes shape.
const MAX_PATH_BYTES: usize = 4_096;
const MAX_COMPONENT_BYTES: usize = 255;

/// Canonical portable ASCII resource, lexical only. The empty resource denotes root.
/// This value never opens a file or proves confinement against links or races.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Resource(String);
impl Resource {
    pub const ROOT: Self = Self(String::new());
    pub fn parse(text: &str) -> Result<Self> {
        if text.is_empty() {
            return Ok(Self::ROOT);
        }
        if !text.is_ascii() || text.len() > MAX_PATH_BYTES {
            return Err(invalid());
        }
        for component in text.split('/') {
            validate_component(component)?;
        }
        Ok(Self(text.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Kind {
    All,
    Exact,
    Subtree,
}

/// Canonical resource text is recorded verbatim in the audit record of every grant that
/// carries it, so a scope whose text cannot fit one audit metadata value is refused here,
/// as a plain typed validation error. Letting it through would only fail later — after the
/// mutation, at audit-append time — rolling back a change the caller was entitled to make and
/// reporting it as an infrastructure error. `ResourceScope` is ASCII-only, so one byte is one
/// UTF-16 unit. `Resource` itself is not bound this way: it only names the thing being
/// authorized and is never recorded.
const MAX_SCOPE_TEXT_UNITS: usize = MAX_METADATA_VALUE_UNITS;

/// Exact path, subtree (including its own root), or all resources. Allows are
/// case-sensitive; denies/intersection are ASCII-insensitive and conservative.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ResourceScope {
    kind: Kind,
    path: Resource,
}
impl ResourceScope {
    pub const ALL: Self = Self {
        kind: Kind::All,
        path: Resource::ROOT,
    };
    pub fn parse(text: &str) -> Result<Self> {
        if text == "*" {
            return Ok(Self::ALL);
        }
        if text.len() > MAX_SCOPE_TEXT_UNITS {
            return Err(invalid());
        }
        let (kind, path) = match text.strip_suffix("/**") {
            Some(path) => (Kind::Subtree, path),
            None => (Kind::Exact, text),
        };
        let path = Resource::parse(path)?;
        if path.is_root() {
            return Err(invalid());
        }
        Ok(Self { kind, path })
    }
    pub const fn is_all(&self) -> bool {
        matches!(self.kind, Kind::All)
    }
    pub fn text(&self) -> String {
        match self.kind {
            Kind::All => "*".into(),
            Kind::Exact => self.path.as_str().into(),
            Kind::Subtree => format!("{}/**", self.path.as_str()),
        }
    }
    pub fn allows(&self, resource: &Resource) -> bool {
        self.matches(resource, false)
    }
    pub fn denies(&self, resource: &Resource) -> bool {
        self.matches(resource, true)
    }
    fn matches(&self, resource: &Resource, ignore_case: bool) -> bool {
        if self.is_all() {
            return true;
        }
        let path = self.path.as_str();
        let resource = resource.as_str();
        if equals(resource, path, ignore_case) {
            return true;
        }
        self.kind == Kind::Subtree && descendant(resource, path, ignore_case)
    }
    /// A single allow's case-sensitive containment; partial allows are not unioned.
    pub fn covers(&self, other: &Self) -> bool {
        match (self.kind, other.kind) {
            (Kind::All, _) => true,
            (_, Kind::All) => false,
            (Kind::Exact, Kind::Exact) => self.path == other.path,
            (Kind::Exact, Kind::Subtree) => false,
            (Kind::Subtree, _) => self.allows(&other.path),
        }
    }
    /// Conservative deny intersection at component boundaries.
    pub fn overlaps(&self, other: &Self) -> bool {
        self.is_all() || other.is_all() || self.denies(&other.path) || other.denies(&self.path)
    }
}
impl std::fmt::Display for ResourceScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text())
    }
}

fn equals(left: &str, right: &str, ignore_case: bool) -> bool {
    if ignore_case {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}
fn descendant(resource: &str, path: &str, ignore_case: bool) -> bool {
    resource.as_bytes().get(path.len()) == Some(&b'/')
        && resource
            .get(..path.len())
            .is_some_and(|prefix| equals(prefix, path, ignore_case))
}
fn validate_component(component: &str) -> Result<()> {
    if component.is_empty()
        || component.len() > MAX_COMPONENT_BYTES
        || matches!(component, "." | "..")
        || component.starts_with(' ')
        || component.ends_with(['.', ' '])
        || component
            .bytes()
            .any(|b| b.is_ascii_control() || b"<>:\"|?*\\".contains(&b))
    {
        return Err(invalid());
    }
    // Win32 device aliases also apply before extensions. Reject space-obscured
    // basenames rather than accepting an alias the OS might interpret differently.
    let base = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) || (base.len() == 4
        && (base.starts_with("COM") || base.starts_with("LPT"))
        && matches!(base.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(invalid());
    }
    Ok(())
}
fn invalid() -> ControlError {
    ControlError::rejected(IdentityErrorCode::InvalidRequest)
}
