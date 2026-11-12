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
    terminal.show(&ui::View {
        step: 1,
        heading: format!("Verifying Vesta {tag}"),
        description: vec![
            "The selected package must match its trusted signature, platform and release identity."
                .into(),
        ],
        rows: vec![],
        hint: String::new(),
    })?;
    let release = release::official::load(&tag, RELEASE_KEY.trim()).inspect_err(|code| {
        let _ = terminal.show(&engine::failure_view(code));
    })?;
    let answers = match headless_answers {
        Some(answers) => answers,
        None => wizard::ask_for_release(&mut terminal, Some(&tag))?,
    };
    wizard::prepare(&mut terminal, &answers, &release)
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
