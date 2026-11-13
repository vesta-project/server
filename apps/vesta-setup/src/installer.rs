//! Standalone installer UI. The selected package owns its matched host runtime.
use crate::{engine, release, ui, wizard};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

const RELEASE_KEY: &str = include_str!("../../../scripts/bootstrap/release-public-key.txt");

pub fn run(args: &[String]) -> engine::Result<()> {
    let mut values = BTreeMap::new();
    let mut flags = BTreeSet::new();
    let mut i = 0;
    while i < args.len() {
        let key = args[i].as_str();
        match key {
            "--help" | "-h" if args.len() == 1 => {
                println!(
                    "Vesta installer\n  vesta-setup install [--channel stable|nightly | --tag RELEASE]\n    [--answers JSON --yes] [--no-color] [--ascii]\n\nWithout a release selection, choose the channel and version interactively."
                );
                return Ok(());
            }
            "--yes" | "--no-color" | "--ascii" => {
                if !flags.insert(key) {
                    return Err("setup_duplicate_option".into());
                }
            }
            "--channel" | "--tag" | "--answers" => {
                i += 1;
                let value = args
                    .get(i)
                    .filter(|s| !s.is_empty() && !s.starts_with("--"))
                    .ok_or("setup_missing_option_value")?;
                if values.insert(key, value).is_some() {
                    return Err("setup_duplicate_option".into());
                }
            }
            _ => return Err("setup_unknown_option".into()),
        }
        i += 1;
    }
    if values.contains_key("--channel") && values.contains_key("--tag") {
        return Err("setup_ambiguous_release_selection".into());
    }
    if values
        .get("--channel")
        .is_some_and(|c| !matches!(c.as_str(), "stable" | "nightly"))
    {
        return Err("setup_invalid_release_channel".into());
    }
    if values
        .get("--tag")
        .is_some_and(|tag| !release::official::valid_tag(tag))
    {
        return Err("setup_invalid_release_tag".into());
    }
    if flags.contains("--yes") != values.contains_key("--answers") {
        return Err("setup_headless_requires_answers_and_yes".into());
    }
    let mut terminal =
        wizard::Terminal::new(!flags.contains("--no-color"), !flags.contains("--ascii"));
    if !terminal.is_interactive() && !values.contains_key("--answers") {
        return Err("setup_terminal_required_use_answers_and_yes".into());
    }
    if values.contains_key("--answers")
        && !values.contains_key("--channel")
        && !values.contains_key("--tag")
    {
        return Err("setup_headless_requires_release_selection".into());
    }
    let headless_answers = values
        .get("--answers")
        .map(|path| engine::read_answers(&PathBuf::from(path)))
        .transpose()?;
    let tag = match values.get("--tag") {
        Some(tag) => (*tag).clone(),
        None => {
            terminal.show(&ui::View {
                step: 1,
                heading: "Finding Vesta releases".into(),
                description: vec![
                    "Reading the public release catalog. No installation files are changed.".into(),
                ],
                rows: vec![],
                hint: String::new(),
            })?;
            let entries = release::official::catalog()?;
            match values.get("--channel") {
                Some(channel) => {
                    release::official::latest(&entries, channel.as_str() == "nightly")?.to_owned()
                }
                None => wizard::select_package(&mut terminal, &entries)?,
            }
        }
    };
    let release = verify_official(&mut terminal, &tag, 1).inspect_err(|code| {
        let _ = terminal.show(&engine::failure_view(code));
    })?;
    let selection = match headless_answers {
        Some(answers) => wizard::SetupSelection {
            answers,
            resume: None,
        },
        None => wizard::ask_installation(&mut terminal, Some(&tag))?,
    };
    let release = resolve_resume_release(&selection, release, |tag| {
        verify_official(&mut terminal, tag, 5)
    })
    .inspect_err(|code| {
        let _ = terminal.show(&engine::failure_view(code));
    })?;
    wizard::prepare(&mut terminal, &selection.answers, &release)
}

fn verify_official(
    terminal: &mut wizard::Terminal,
    tag: &str,
    step: usize,
) -> engine::Result<release::Release> {
    terminal.show(&ui::View {
        step,
        heading: format!("Verifying Vesta {tag}"),
        description: vec![
            "The package must match its trusted signature, platform and exact release identity."
                .into(),
        ],
        rows: vec![],
        hint: String::new(),
    })?;
    release::official::load(tag, RELEASE_KEY.trim())
}

/// Resolve an explicitly confirmed interactive resume. Headless/pinned callers
/// retain their explicit selection and the engine's exact receipt checks.
pub fn resolve_resume_release(
    selection: &wizard::SetupSelection,
    selected: release::Release,
    load: impl FnOnce(&str) -> engine::Result<release::Release>,
) -> engine::Result<release::Release> {
    selected.ensure_intact()?;
    let Some(receipt) = &selection.resume else {
        return Ok(selected);
    };
    if selection.answers != receipt.answers {
        return Err("setup_resume_settings_mismatch".into());
    }
    let matches = |release: &release::Release| {
        release.manifest.version == receipt.release_version
            && release.manifest_hash() == receipt.release_manifest_sha256
    };
    if matches(&selected) {
        return Ok(selected);
    }
    let restored = load(&receipt.release_version)?;
    restored.ensure_intact()?;
    if !matches(&restored) || restored.release_key() != selected.release_key() {
        return Err("setup_resume_release_mismatch".into());
    }
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_and_ambiguous_selection_before_network_or_effects() {
        for args in [
            vec!["--tag", "../main"],
            vec!["--channel", "other"],
            vec!["--channel", "nightly", "--tag", "v1"],
            vec!["--yes"],
            vec!["--answers", "missing", "--yes"],
            vec!["--bundle-url", "https://example.invalid"],
        ] {
            assert!(run(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
    }
}
