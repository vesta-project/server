//! `.vesta-backupignore`: a bounded gitignore-style subset.
//!
//! - Blank lines and `#` comments are ignored; `\#` and `\!` escape a leading character.
//! - `!pattern` re-includes a path excluded by an earlier pattern; the last match wins.
//! - A pattern containing `/` (other than a trailing one) is anchored to the game root;
//!   otherwise it matches the file or directory name at any depth.
//! - A trailing `/` matches directories only (and so everything under them).
//! - `*` matches within one path segment, `?` one character, `**` any number of
//!   segments. Character classes are not supported and are matched literally.
pub const MAX_PATTERNS: usize = 1000;
const MAX_LINE: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Rule {
    pattern: Vec<String>,
    anchored: bool,
    directory_only: bool,
    negate: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ignore {
    rules: Vec<Rule>,
    include: Vec<String>,
}

impl Ignore {
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let mut rules = Vec::new();
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r').trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.len() > MAX_LINE || rules.len() >= MAX_PATTERNS {
                return Err("backup_ignore_file_invalid");
            }
            let (negate, line) = match line.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, line.strip_prefix('\\').unwrap_or(line)),
            };
            let (directory_only, line) = match line.strip_suffix('/') {
                Some(rest) => (true, rest),
                None => (false, line),
            };
            let anchored = line.contains('/');
            let line = line.strip_prefix('/').unwrap_or(line);
            if line.is_empty() || line.split('/').any(|s| s.is_empty() || s == "..") {
                return Err("backup_ignore_file_invalid");
            }
            rules.push(Rule {
                pattern: line.split('/').map(str::to_owned).collect(),
                anchored,
                directory_only,
                negate,
            });
        }
        Ok(Self {
            rules,
            include: Vec::new(),
        })
    }

    pub fn with_selection(
        mut self,
        include: &[String],
        exclude: &[String],
    ) -> Result<Self, &'static str> {
        if !valid_selection(include, exclude) {
            return Err("backup_ignore_file_invalid");
        }
        self.include = include.to_vec();
        self.rules.extend(Self::parse(&exclude.join("\n"))?.rules);
        Ok(self)
    }

    /// Whether a root-relative `/`-separated path is excluded.
    pub fn excluded(&self, path: &str, is_dir: bool) -> bool {
        if !self.include.is_empty()
            && !self.include.iter().any(|root| {
                path == root
                    || path
                        .strip_prefix(root.as_str())
                        .is_some_and(|tail| tail.starts_with('/'))
                    || (is_dir
                        && root
                            .strip_prefix(path)
                            .is_some_and(|tail| tail.starts_with('/')))
            })
        {
            return true;
        }
        let segments: Vec<&str> = path.split('/').collect();
        let mut excluded = false;
        for rule in &self.rules {
            if rule.directory_only && !is_dir {
                continue;
            }
            let matched = if rule.anchored {
                matches(&rule.pattern, &segments)
            } else {
                segments
                    .last()
                    .is_some_and(|name| glob(&rule.pattern[0], name))
            };
            if matched {
                excluded = !rule.negate;
            }
        }
        excluded
    }
}

/// Match pattern segments against path segments, with `**` spanning segments.
fn matches(pattern: &[String], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=path.len()).any(|skip| matches(rest, &path[skip..]))
        }
        Some((first, rest)) => path
            .split_first()
            .is_some_and(|(segment, tail)| glob(first, segment) && matches(rest, tail)),
    }
}

/// `*` and `?` within one segment.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Roots are literal paths; excludes are bounded single-line ignore patterns.
pub fn valid_selection(include: &[String], exclude: &[String]) -> bool {
    include.len() <= 64
        && exclude.len() <= 64
        && include.iter().all(|p| {
            crate::files::RelativePath::parse(p).is_ok() && crate::backups::engine::literal_path(p)
        })
        && exclude.iter().all(|p| {
            !p.is_empty()
                && p.len() <= MAX_LINE
                && !p.chars().any(char::is_control)
                && !p.starts_with(['!', '#', '\\'])
                && Ignore::parse(p).is_ok()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitignore_subset_semantics() {
        let ignore = Ignore::parse(
            "# comment\n*.log\nlogs/\n/cache\nworld/**/*.tmp\n!important.log\ncrash-reports/\n",
        )
        .unwrap();
        assert!(ignore.excluded("latest.log", false));
        assert!(ignore.excluded("plugins/x/debug.log", false));
        assert!(!ignore.excluded("important.log", false));
        assert!(ignore.excluded("logs", true));
        assert!(!ignore.excluded("logs", false));
        assert!(ignore.excluded("cache", true));
        assert!(!ignore.excluded("plugins/cache", true));
        assert!(ignore.excluded("world/region/a.tmp", false));
        assert!(ignore.excluded("world/a.tmp", false));
        assert!(!ignore.excluded("world/region/r.0.0.mca", false));
        assert!(ignore.excluded("crash-reports", true));
    }

    #[test]
    fn malformed_and_oversized_files_refuse() {
        assert!(Ignore::parse("../escape\n").is_err());
        assert!(Ignore::parse("a//b\n").is_err());
        assert!(Ignore::parse(&"x\n".repeat(MAX_PATTERNS + 1)).is_err());
        assert!(Ignore::parse(&"y".repeat(MAX_LINE + 1)).is_err());
        assert!(glob("r.*.mca", "r.0.0.mca"));
        assert!(!glob("r.?.mca", "r.10.mca"));
    }
}
