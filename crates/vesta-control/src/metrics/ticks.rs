//! Closed opt-in console adapters. These are format-filtered observations, not
//! causal proof: Minecraft console output has no request ID. No probes/commands
//! are guessed from a status ping's untrusted version.name.
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use vesta_game_runtime::{
    process::{GracefulStop, LaunchSpec},
    readiness::server_info_message,
};

pub(crate) const WINDOW: Duration = Duration::from_secs(3);
pub(crate) const MAX_LINES: usize = 128;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Adapter {
    PaperMspt,
    PaperTps,
    Spigot,
    Vanilla,
    Forge,
    NeoForge,
}
impl Adapter {
    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::PaperMspt => "mspt",
            Self::PaperTps | Self::Spigot => "tps",
            Self::Vanilla => "tick query",
            Self::Forge => "forge tps",
            Self::NeoForge => "neoforge tps",
        }
    }
    pub(crate) fn next(self) -> Self {
        match self {
            Self::PaperMspt => Self::PaperTps,
            Self::PaperTps => Self::PaperMspt,
            other => other,
        }
    }
    fn select(software: &str, version: &str) -> Option<Self> {
        let parts = version
            .split('.')
            .map(|v| v.parse::<u32>().ok())
            .collect::<Option<Vec<_>>>()?;
        if !(2..=3).contains(&parts.len()) || parts[0] != 1 || !(16..=21).contains(&parts[1]) {
            return None;
        }
        let patch = parts.get(2).copied().unwrap_or(0);
        match software {
            "paper" | "purpur" | "pufferfish" | "leaf" => Some(Self::PaperMspt),
            "spigot" | "bukkit" => Some(Self::Spigot),
            "vanilla" | "fabric" | "quilt" if parts[1] > 20 || (parts[1] == 20 && patch >= 3) => {
                Some(Self::Vanilla)
            }
            "forge" => Some(Self::Forge),
            "neoforge" if parts[1] > 20 || (parts[1] == 20 && patch >= 2) => Some(Self::NeoForge),
            _ => None, // Folia's region clocks have no honest server-wide aggregate.
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Observation {
    pub tps: Option<f64>,
    pub mspt: Option<f64>,
}
#[derive(Clone)]
pub(crate) struct Detection {
    adapter: Option<Adapter>,
    reason: &'static str,
}
impl Detection {
    #[cfg(test)]
    pub(crate) fn fixture(adapter: Adapter) -> Self {
        Self {
            adapter: Some(adapter),
            reason: "fixture",
        }
    }
    pub(crate) fn unknown() -> Self {
        Self {
            adapter: None,
            reason: "server software not identified",
        }
    }
    pub(crate) fn adapter(&self) -> Option<Adapter> {
        self.adapter
    }
    pub(crate) fn reason(&self) -> &'static str {
        self.reason
    }
}
// A narrow read-only projection, deliberately not an install/provider dependency.
#[derive(Deserialize)]
struct Receipt {
    schema: u32,
    software: String,
    version: String,
    launch: Recipe,
}
#[derive(Deserialize)]
struct Recipe {
    kind: String,
    java: PathBuf,
    working_directory: PathBuf,
    artifact: Option<PathBuf>,
    argfile: Option<PathBuf>,
    args: Vec<String>,
    stop_mode: String,
}
fn plain(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_symlink())
}
pub(crate) fn detect(spec: &LaunchSpec, game: &Path) -> Detection {
    let unknown = Detection::unknown();
    let Some(instance) = game.parent() else {
        return unknown;
    };
    let control = instance.join("control");
    let path = control.join("receipt.json");
    if !plain(instance) || !plain(game) || !plain(&control) || !plain(&path) {
        return unknown;
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    let Ok(file) = std::fs::File::open(&path) else {
        return unknown;
    };
    if file.take(65_537).read_to_end(&mut bytes).is_err() || bytes.len() > 65_536 {
        return unknown;
    }
    let Ok(receipt) = serde_json::from_slice::<Receipt>(&bytes) else {
        return unknown;
    };
    let r = receipt.launch;
    if !matches!(receipt.schema, 1 | 2)
        || r.java != spec.literal_program()
        || r.working_directory != game
        || r.working_directory != spec.working_directory()
        || r.stop_mode != "console-stop"
        || spec.graceful_stop() != GracefulStop::MinecraftCommand
        || r.args.len() > 256
    {
        return unknown;
    }
    // Compare the literal argv suffix, not jar filenames, process names, or a
    // status-ping brand. JVM flags may precede this suffix; no alternate launch
    // selector (-jar, @argfile, classpath/module) may occur in that prefix.
    let mut expected = Vec::new();
    match (receipt.schema, r.kind.as_str(), r.artifact, r.argfile) {
        (1, "java-jar", Some(artifact), None) if artifact.is_absolute() => {
            expected.push("-jar".to_owned());
            let Some(path) = artifact.to_str() else {
                return unknown;
            };
            expected.push(path.to_owned());
        }
        (2, "java-argfile", None, Some(argfile)) if argfile.is_absolute() => {
            let Some(path) = argfile.to_str() else {
                return unknown;
            };
            expected.push(format!("@{path}"));
        }
        _ => return unknown,
    }
    expected.extend(r.args);
    let args = spec.literal_args();
    let Some(prefix) = args.len().checked_sub(expected.len()) else {
        return unknown;
    };
    if args[prefix..] != expected
        || args[..prefix].iter().any(|a| {
            !a.starts_with('-')
                || a.starts_with('@')
                || matches!(
                    a.as_str(),
                    "-jar" | "-cp" | "-classpath" | "--class-path" | "-m" | "--module"
                )
        })
    {
        return unknown;
    }
    match Adapter::select(&receipt.software, &receipt.version) {
        Some(adapter) => Detection {
            adapter: Some(adapter),
            reason: "command polling awaits an observation",
        },
        None => Detection {
            adapter: None,
            reason: if receipt.software == "folia" {
                "region-based tick clocks have no server-wide aggregate"
            } else if matches!(
                receipt.software.as_str(),
                "paper"
                    | "purpur"
                    | "pufferfish"
                    | "leaf"
                    | "spigot"
                    | "bukkit"
                    | "vanilla"
                    | "fabric"
                    | "quilt"
                    | "forge"
                    | "neoforge"
            ) {
                "version not in the tested adapter matrix"
            } else {
                "server software not identified"
            },
        },
    }
}
fn decimal(value: &str, max: f64) -> Option<f64> {
    if value.is_empty()
        || !value.bytes().all(|c| c.is_ascii_digit() || c == b'.')
        || value.bytes().filter(|c| *c == b'.').count() > 1
    {
        return None;
    }
    let value = value.parse::<f64>().ok()?;
    (value.is_finite() && (0.0..=max).contains(&value)).then_some(value)
}
fn colors(message: &str) -> Option<String> {
    let mut result = String::new();
    let mut chars = message.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            let code = chars.next()?;
            if !"0123456789abcdefklmnorxABCDEFKLMNORX".contains(code) {
                return None;
            }
        } else if c.is_control() {
            return None;
        } else {
            result.push(c);
        }
    }
    Some(result)
}
pub(crate) struct Parser {
    adapter: Adapter,
    header: bool,
}
impl Parser {
    pub(crate) fn new(adapter: Adapter) -> Self {
        Self {
            adapter,
            header: false,
        }
    }
    pub(crate) fn line(&mut self, line: &str) -> Option<Observation> {
        let message = colors(server_info_message(line)?)?;
        match self.adapter {
            Adapter::PaperTps | Adapter::Spigot => {
                let body = message.strip_prefix("TPS from last 1m, 5m, 15m: ")?;
                let numbers = body
                    .split(", ")
                    .map(|v| decimal(v.strip_prefix('*').unwrap_or(v), 1000.0))
                    .collect::<Option<Vec<_>>>()?;
                if numbers.len() != 3 {
                    return None;
                }
                Some(Observation {
                    tps: Some(numbers[0].min(20.0)),
                    mspt: None,
                })
            }
            Adapter::PaperMspt => {
                if message == "Server tick times (avg/min/max) from last 5s, 10s, 1m:" {
                    self.header = true;
                    return None;
                }
                if !self.header {
                    return None;
                }
                let body = message.strip_prefix("◴ ")?;
                let groups = body
                    .split(", ")
                    .map(|group| {
                        group
                            .split('/')
                            .map(|v| decimal(v, 3_600_000.0))
                            .collect::<Option<Vec<_>>>()
                    })
                    .collect::<Option<Vec<_>>>()?;
                if groups.len() != 3
                    || groups
                        .iter()
                        .any(|g| g.len() != 3 || g[1] > g[0] || g[0] > g[2])
                {
                    return None;
                }
                Some(Observation {
                    tps: None,
                    mspt: Some(groups[2][0]),
                })
            }
            Adapter::Forge | Adapter::NeoForge => {
                let body = message.strip_prefix("Overall: Mean tick time: ")?;
                let (mspt, tps) = body.split_once(" ms. Mean TPS: ")?;
                Some(Observation {
                    mspt: Some(decimal(mspt, 3_600_000.0)?),
                    tps: Some(decimal(tps, 1000.0)?),
                })
            }
            Adapter::Vanilla => {
                if let Some(rate) = message
                    .strip_prefix("Target tick rate: ")
                    .and_then(|v| v.strip_suffix(" per second."))
                {
                    decimal(rate, 1000.0)?;
                    self.header = true;
                    return None;
                }
                if !self.header {
                    return None;
                }
                let body = message.strip_prefix("Average time per tick: ")?;
                let (avg, target) = body.split_once("ms (Target: ")?;
                decimal(target.strip_suffix("ms)")?, 3_600_000.0)?;
                // The target rate is NOT measured achieved TPS. Never relabel it.
                Some(Observation {
                    mspt: Some(decimal(avg, 3_600_000.0)?),
                    tps: None,
                })
            }
        }
    }
}
/// Pure post-write correlation horizon used by the owned driver. Unknown/truncated
/// or skipped lines fail closed. A timeout never starts a retry in this window.
pub(crate) struct Correlation {
    pub cursor: u64,
    started: Instant,
    lines: usize,
    parser: Parser,
}
impl Correlation {
    pub(crate) fn new(adapter: Adapter, cursor: u64, started: Instant) -> Self {
        Self {
            cursor,
            started,
            lines: 0,
            parser: Parser::new(adapter),
        }
    }
    pub(crate) fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= WINDOW || self.lines >= MAX_LINES
    }
    pub(crate) fn line(
        &mut self,
        cursor: u64,
        truncated: bool,
        text: &str,
        now: Instant,
    ) -> Result<Option<Observation>, ()> {
        if self.expired(now) || truncated || cursor != self.cursor.saturating_add(1) {
            return Err(());
        }
        self.cursor = cursor;
        self.lines += 1;
        Ok(self.parser.line(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(body: &str) -> String {
        format!("[12:00:00 INFO]: {body}")
    }
    #[test]
    fn tick_adapters_parse_color_clamped_paper_and_overall_forge_only() {
        let mut tps = Parser::new(Adapter::PaperTps);
        assert_eq!(
            tps.line(&line("§aTPS from last 1m, 5m, 15m: §a*20.0§r, *20.0, 19.5"))
                .unwrap()
                .tps,
            Some(20.0)
        );
        let mut mspt = Parser::new(Adapter::PaperMspt);
        assert!(
            mspt.line(&line("◴ 12.3/8.1/45.7, 11.9/7.6/52.0, 13.0/7.2/88.4"))
                .is_none()
        );
        assert!(
            mspt.line(&line(
                "Server tick times (avg/min/max) from last 5s, 10s, 1m:"
            ))
            .is_none()
        );
        assert_eq!(
            mspt.line(&line("◴ 12.3/8.1/45.7, 11.9/7.6/52.0, 13.0/7.2/88.4"))
                .unwrap()
                .mspt,
            Some(13.0)
        );
        for adapter in [Adapter::Forge, Adapter::NeoForge] {
            let mut parser = Parser::new(adapter);
            assert!(
                parser
                    .line(&line("Dim 0: Mean tick time: 2.0 ms. Mean TPS: 20.0"))
                    .is_none()
            );
            assert_eq!(
                parser.line(&line("Overall: Mean tick time: 30.5 ms. Mean TPS: 19.9")),
                Some(Observation {
                    mspt: Some(30.5),
                    tps: Some(19.9)
                })
            );
        }
        let mut spigot = Parser::new(Adapter::Spigot);
        assert_eq!(
            spigot
                .line(&line("TPS from last 1m, 5m, 15m: 18.5, 19.0, 20.0"))
                .unwrap()
                .tps,
            Some(18.5)
        );
    }
    #[test]
    fn tick_vanilla_reports_measured_mspt_not_target_tps_and_refuses_locale_and_chat() {
        let mut parser = Parser::new(Adapter::Vanilla);
        assert!(
            parser
                .line(&line("Target tick rate: 20.0 per second."))
                .is_none()
        );
        assert_eq!(
            parser.line(&line("Average time per tick: 14.3ms (Target: 50.0ms)")),
            Some(Observation {
                mspt: Some(14.3),
                tps: None
            })
        );
        assert!(
            parser
                .line(&line("Average time per tick: 14,3ms (Target: 50,0ms)"))
                .is_none()
        );
        assert!(
            parser
                .line(&line(
                    "<Alex> Average time per tick: 14.3ms (Target: 50.0ms)"
                ))
                .is_none()
        );
        assert!(
            parser
                .line("Average time per tick: 14.3ms (Target: 50.0ms)")
                .is_none()
        );
        assert!(
            Parser::new(Adapter::Spigot)
                .line(&line("<Alex> TPS from last 1m, 5m, 15m: 20.0, 20.0, 20.0"))
                .is_none()
        );
    }
    #[test]
    fn tick_selection_is_versioned_folia_older_and_unknown_are_unknown() {
        for software in [
            "paper",
            "purpur",
            "pufferfish",
            "leaf",
            "spigot",
            "bukkit",
            "forge",
        ] {
            assert!(Adapter::select(software, "1.21.1").is_some());
        }
        for software in ["vanilla", "fabric", "quilt"] {
            assert!(Adapter::select(software, "1.20.2").is_none());
            assert_eq!(Adapter::select(software, "1.20.3"), Some(Adapter::Vanilla));
        }
        assert!(Adapter::select("folia", "1.21.1").is_none());
        assert!(Adapter::select("paper", "1.8.8").is_none());
        assert!(Adapter::select("unknown", "1.21.1").is_none());
        assert!(Adapter::select("paper", "1.22").is_none());
        assert!(Adapter::select("paper", "26.1").is_none());
    }
    #[test]
    fn tick_correlation_window_timeout_gap_and_line_budget_fail_closed() {
        let at = Instant::now();
        let mut c = Correlation::new(Adapter::Spigot, 10, at);
        assert!(
            c.line(
                10,
                false,
                &line("TPS from last 1m, 5m, 15m: 20.0, 20.0, 20.0"),
                at
            )
            .is_err()
        );
        let mut c = Correlation::new(Adapter::Spigot, 10, at);
        assert!(
            c.line(
                11,
                false,
                &line("TPS from last 1m, 5m, 15m: 20.0, 20.0, 20.0"),
                at + WINDOW
            )
            .is_err()
        );
        let mut c = Correlation::new(Adapter::Spigot, 10, at);
        for cursor in 11..=138 {
            assert_eq!(c.line(cursor, false, &line("unrelated"), at), Ok(None));
        }
        assert!(c.line(139, false, &line("unrelated"), at).is_err());
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    #[test]
    fn tick_detection_requires_bounded_receipt_and_matching_literal_launch() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        let control = root.path().join("control");
        std::fs::create_dir(&game).unwrap();
        std::fs::create_dir(&control).unwrap();
        let java = std::env::current_exe().unwrap();
        let artifact = root.path().join("software/paper.jar");
        let args = vec![
            "-Xmx1G".into(),
            "-jar".into(),
            artifact.to_str().unwrap().into(),
            "nogui".into(),
        ];
        let spec = LaunchSpec::new(
            java.clone(),
            args,
            game.clone(),
            GracefulStop::MinecraftCommand,
        )
        .unwrap();
        assert_eq!(
            detect(&spec, &game).reason(),
            "server software not identified"
        );
        let path = control.join("receipt.json");
        let mut receipt = serde_json::json!({"schema":1,"software":"paper","version":"1.21.1","launch":{"kind":"java-jar","java":java,"working_directory":game,"artifact":artifact,"args":["nogui"],"stop_mode":"console-stop"}});
        std::fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert_eq!(detect(&spec, &game).adapter(), Some(Adapter::PaperMspt));
        receipt["launch"]["args"] = serde_json::json!(["other"]);
        std::fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(detect(&spec, &game).adapter().is_none());
        receipt["launch"]["args"] = serde_json::json!(["nogui"]);
        receipt["software"] = serde_json::json!("folia");
        std::fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(detect(&spec, &game).adapter().is_none());
        assert!(detect(&spec, &game).reason().contains("region-based"));
        std::fs::write(&path, vec![b' '; 65_537]).unwrap();
        assert!(detect(&spec, &game).adapter().is_none());
    }
}
