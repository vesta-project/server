//! One active prompt, inline redraw, arrow keys, back navigation and plain fallback.
use crate::{
    engine::{self, Answers, Result, Startup},
    network::{self, ExternalProbe},
    paths, platform,
    ui::{self, Row, TaskStatus, Tone, View},
};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, ClearType},
};
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

pub struct Terminal {
    pub theme: ui::Theme,
    interactive: bool,
    lines: u16,
    last: Option<Instant>,
}
impl Terminal {
    pub fn new(color: bool, unicode: bool) -> Self {
        let interactive = io::stdin().is_terminal()
            && io::stdout().is_terminal()
            && std::env::var("TERM").as_deref() != Ok("dumb");
        let width = terminal::size().map_or(88, |(w, _)| usize::from(w));
        Self {
            theme: ui::Theme {
                color: color
                    && std::env::var_os("NO_COLOR").is_none()
                    && io::stdout().is_terminal(),
                unicode,
                width: width.clamp(40, 100),
            },
            interactive,
            lines: 0,
            last: None,
        }
    }
    pub fn show(&mut self, view: &View) -> Result<()> {
        if self.interactive {
            let width = terminal::size().map_or(88, |(w, _)| usize::from(w));
            self.theme.width = width.clamp(40, 100);
        }
        // Download callbacks are per chunk. Render at most ten times per second.
        if view.heading == "Preparing your host"
            && self
                .last
                .is_some_and(|t| t.elapsed() < Duration::from_millis(100))
        {
            return Ok(());
        }
        let text = ui::render(view, self.theme.clone());
        let mut out = io::stdout().lock();
        if self.interactive && self.lines > 0 {
            execute!(
                out,
                cursor::MoveUp(self.lines),
                cursor::MoveToColumn(0),
                terminal::Clear(ClearType::FromCursorDown)
            )
            .map_err(|_| "setup_terminal_unavailable")?;
        }
        let output = if terminal::is_raw_mode_enabled().unwrap_or(false) {
            text.replace('\n', "\r\n")
        } else {
            text.clone()
        };
        out.write_all(output.as_bytes())
            .and_then(|_| out.flush())
            .map_err(|_| "setup_terminal_unavailable")?;
        self.lines = text.lines().count().try_into().unwrap_or(u16::MAX);
        self.last = Some(Instant::now());
        Ok(())
    }
}
struct Raw;
impl Raw {
    fn begin() -> Result<Self> {
        terminal::enable_raw_mode().map_err(|_| "setup_terminal_unavailable")?;
        let guard = Self;
        execute!(io::stdout(), cursor::Hide, event::EnableBracketedPaste)
            .map_err(|_| "setup_terminal_unavailable")?;
        Ok(guard)
    }
}
impl Drop for Raw {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), cursor::Show, event::DisableBracketedPaste);
    }
}
enum Answer<T> {
    Next(T),
    Back,
}
fn key() -> Result<Event> {
    event::read().map_err(|_| "setup_terminal_unavailable".into())
}
fn choose(terminal: &mut Terminal, mut view: View, initial: usize) -> Result<Answer<usize>> {
    let disabled: Vec<_> = view
        .rows
        .iter()
        .filter_map(|r| {
            if let Row::Choice { disabled, .. } = r {
                Some(*disabled)
            } else {
                None
            }
        })
        .collect();
    if disabled.is_empty() {
        return Err("setup_prompt_invalid".into());
    }
    let mut selected = initial.min(disabled.len().saturating_sub(1));
    let mut render = |terminal: &mut Terminal, selected: usize| {
        let mut i = 0;
        for row in &mut view.rows {
            if let Row::Choice {
                selected: active, ..
            } = row
            {
                *active = i == selected;
                i += 1;
            }
        }
        terminal.show(&view)
    };
    if !terminal.interactive {
        return Err("setup_terminal_required_use_answers_and_yes".into());
    }
    let _raw = Raw::begin()?;
    render(terminal, selected)?;
    loop {
        match key()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err("setup_cancelled".into());
                }
                KeyCode::Esc => return Ok(Answer::Back),
                KeyCode::Up | KeyCode::BackTab => {
                    selected = if selected == 0 {
                        disabled.len() - 1
                    } else {
                        selected - 1
                    };
                }
                KeyCode::Down | KeyCode::Tab => selected = (selected + 1) % disabled.len(),
                KeyCode::Enter if !disabled[selected] => return Ok(Answer::Next(selected)),
                KeyCode::Char(c) if ('1'..='9').contains(&c) => {
                    let i = c as usize - '1' as usize;
                    if i < disabled.len() {
                        selected = i;
                    }
                }
                _ => {}
            },
            Event::Resize(_, _) => {}
            _ => continue,
        }
        render(terminal, selected)?;
    }
}
fn input(
    terminal: &mut Terminal,
    step: usize,
    heading: &str,
    label: &str,
    initial: String,
    validate: impl Fn(&str) -> Result<()>,
) -> Result<Answer<String>> {
    let _raw = Raw::begin()?;
    let mut value = initial;
    let mut error = None;
    loop {
        terminal.show(&input_view(
            step,
            heading,
            label,
            &format!("{value}▏"),
            error.clone(),
        ))?;
        match key()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err("setup_cancelled".into());
                }
                KeyCode::Esc => return Ok(Answer::Back),
                KeyCode::Backspace => {
                    value.pop();
                    error = None;
                }
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.clear();
                    error = None;
                }
                KeyCode::Char(c)
                    if !k
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !c.is_control()
                        && value.len() + c.len_utf8() <= 4096 =>
                {
                    value.push(c);
                    error = None;
                }
                KeyCode::Enter => match validate(&value) {
                    Ok(()) => return Ok(Answer::Next(value)),
                    Err(code) => error = Some(code),
                },
                _ => {}
            },
            Event::Paste(text) => {
                for c in text.chars().filter(|c| !c.is_control()) {
                    if value.len() + c.len_utf8() <= 4096 {
                        value.push(c);
                    }
                }
                error = None;
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}
fn view(step: usize, heading: &str, description: &str) -> View {
    View {
        step,
        heading: heading.into(),
        description: vec![description.into()],
        rows: vec![],
        hint: "↑ ↓ to choose · Enter to continue · Esc to go back · Ctrl+C to cancel".into(),
    }
}
pub fn with_choices(mut view: View, labels: &[(&str, &str, bool)], selected: usize) -> View {
    view.rows.extend(
        labels
            .iter()
            .enumerate()
            .map(|(i, (label, description, disabled))| Row::Choice {
                label: (*label).into(),
                description: (*description).into(),
                selected: i == selected,
                disabled: *disabled,
            }),
    );
    view
}
pub fn input_view(
    step: usize,
    heading: &str,
    label: &str,
    value: &str,
    error: Option<String>,
) -> View {
    View {
        step,
        heading: heading.into(),
        description: vec![],
        rows: vec![Row::Input {
            label: label.into(),
            value: value.into(),
            error,
        }],
        hint: "Enter to continue · Esc to go back · Ctrl+C to cancel".into(),
    }
}
pub fn welcome_view(selected: usize) -> View {
    with_choices(
        view(
            1,
            "Welcome to Vesta",
            "Let's prepare a home for your Minecraft server.",
        ),
        &[
            ("Set up a new host", "", false),
            ("Connect an existing server", "", false),
        ],
        selected,
    )
}
pub fn startup_view(available: bool, selected: usize) -> View {
    with_choices(
        view(
            4,
            "How should Vesta start?",
            "Choose what happens after you sign in to this computer.",
        ),
        &[
            (
                "Start automatically · recommended",
                platform::startup_description(),
                !available,
            ),
            (
                "Start manually",
                "Run now; no automatic startup registration.",
                false,
            ),
        ],
        selected,
    )
}
pub fn resume_view(answers: &Answers, selected: usize) -> View {
    let mut v = view(
        3,
        "Finish your previous setup",
        "This folder contains a Vesta installation. Resume without replacing its identity.",
    );
    v.rows.push(Row::Detail {
        label: "Location".into(),
        value: paths::display(&answers.root),
    });
    with_choices(
        v,
        &[
            (
                "Resume this installation",
                "Use the stored location and startup preferences.",
                false,
            ),
            (
                "Choose another location",
                "Return to the location step.",
                false,
            ),
        ],
        selected,
    )
}
pub fn checks_view(answers: &Answers) -> View {
    let mut v = view(3, "Connection checks", "");
    v.rows = vec![Row::Task {
        label: format!("Port {} is available locally", answers.port),
        status: TaskStatus::Done,
    }];
    if let Some(path) = &answers.existing_server {
        v.rows.push(Row::Detail {
            label: "Existing server detected".into(),
            value: paths::display(path),
        });
    }
    with_choices(v, &[("Continue", "", false)], 0)
}
pub fn port_view(available: bool, selected: usize) -> View {
    let mut v = view(3, "Which port should Vesta use?", "HTTPS connection port");
    if !available {
        v.rows.push(Row::Notice {
            tone: Tone::Warning,
            text: format!("Port {} is already in use.", network::DEFAULT_PORT),
        });
    }
    let mut choices = vec![
        ("Use 8443 · default", "", !available),
        ("Use a custom port", "", false),
    ];
    if !available {
        choices.push(("Recheck port 8443", "", false));
    }
    with_choices(v, &choices, selected)
}
pub fn external_view(url: &str, selected: usize) -> View {
    let mut v = view(
        3,
        "Check external access?",
        "The checker receives your public IP, port and TLS fingerprint.",
    );
    v.rows.push(Row::Detail {
        label: "Checker".into(),
        value: url.into(),
    });
    with_choices(
        v,
        &[
            (
                "Run external check",
                "An unsuccessful check will not block setup.",
                false,
            ),
            ("Skip", "", false),
        ],
        selected,
    )
}
pub fn review_view(answers: &Answers, selected: usize) -> View {
    let mut v = view(
        5,
        "Ready to set up your host",
        "Review your choices before anything is installed.",
    );
    v.rows = vec![
        Row::Detail {
            label: "Install location".into(),
            value: paths::display(&answers.root),
        },
        Row::Detail {
            label: "Startup".into(),
            value: if answers.startup == Startup::Automatic {
                platform::startup_description().into()
            } else {
                "Manual".into()
            },
        },
        Row::Detail {
            label: "Connection".into(),
            value: format!("https://127.0.0.1:{}", answers.port),
        },
    ];
    if let Some(path) = &answers.existing_server {
        v.rows.insert(
            1,
            Row::Detail {
                label: "Existing server".into(),
                value: paths::display(path),
            },
        );
    }
    with_choices(
        v,
        &[("Install Vesta host", "", false), ("Go back", "", false)],
        selected,
    )
}
pub fn ask(terminal: &mut Terminal) -> Result<Answers> {
    if !terminal.interactive {
        return Err("setup_terminal_required_use_answers_and_yes".into());
    }
    let mut stage = 0;
    let mut existing = false;
    let mut answers = Answers {
        schema: 1,
        root: engine::default_root(),
        existing_server: None,
        startup: Startup::Manual,
        port: network::DEFAULT_PORT,
        external_probe: None,
    };
    let probe_url = std::env::var("VESTA_SETUP_PROBE_URL").ok();
    loop {
        match stage {
            0 => match choose(
                terminal,
                welcome_view(usize::from(existing)),
                usize::from(existing),
            )? {
                Answer::Next(i) => {
                    existing = i == 1;
                    answers.existing_server = None;
                    stage = 1;
                }
                Answer::Back => return Err("setup_cancelled".into()),
            },
            1 => {
                if existing && answers.existing_server.is_none() {
                    match input(
                        terminal,
                        2,
                        "Connect an existing server",
                        "Existing Minecraft server folder",
                        String::new(),
                        |text| {
                            let p = PathBuf::from(text);
                            engine::guard(&p)?;
                            if !p.is_dir() {
                                return Err("Choose an existing server directory.".into());
                            }
                            if !["server.properties", "eula.txt", "world", "mods", "plugins"]
                                .iter()
                                .any(|f| p.join(f).exists())
                            {
                                return Err(
                                    "No Minecraft server files were detected in this directory."
                                        .into(),
                                );
                            }
                            Ok(())
                        },
                    )? {
                        Answer::Next(p) => {
                            answers.existing_server = Some(paths::normalize(&PathBuf::from(p)))
                        }
                        Answer::Back => {
                            stage = 0;
                            continue;
                        }
                    }
                }
                match input(
                    terminal,
                    2,
                    "Where should Vesta live?",
                    "Dedicated Vesta installation folder",
                    paths::display(&answers.root),
                    |text| {
                        let mut candidate = answers.clone();
                        candidate.root = PathBuf::from(text);
                        engine::validate_answers(&candidate).map_err(|code| friendly(&code))
                    },
                )? {
                    Answer::Next(p) => {
                        answers.root = paths::normalize(&PathBuf::from(p));
                        stage = 2;
                    }
                    Answer::Back => {
                        if existing {
                            answers.existing_server = None;
                        }
                        stage = 0;
                    }
                }
            }
            2 => {
                if answers.root.join("private").join("setup.json").exists() {
                    let prior = engine::read_receipt(&answers.root)?;
                    match choose(terminal, resume_view(&prior.answers, 0), 0)? {
                        Answer::Next(0) => {
                            answers = prior.answers;
                            stage = 4;
                            continue;
                        }
                        _ => {
                            stage = 1;
                            continue;
                        }
                    }
                }
                let available = engine::port_available(network::DEFAULT_PORT);
                let selected = usize::from(!available || answers.port != network::DEFAULT_PORT);
                match choose(terminal, port_view(available, selected), selected)? {
                    Answer::Next(0) => answers.port = network::DEFAULT_PORT,
                    Answer::Next(1) => match input(
                        terminal,
                        3,
                        "Choose a custom port",
                        "Port (1–65535)",
                        answers.port.to_string(),
                        validate_port,
                    )? {
                        Answer::Next(port) => {
                            answers.port = port.parse().map_err(|_| "setup_port_invalid")?
                        }
                        Answer::Back => continue,
                    },
                    Answer::Next(_) => continue,
                    Answer::Back => {
                        stage = 1;
                        continue;
                    }
                }
                if validate_port(&answers.port.to_string()).is_err() {
                    continue;
                }
                if let Some(url) = &probe_url {
                    match choose(terminal, external_view(url, 0), 0)? {
                        Answer::Next(0) => {
                            let probe = ExternalProbe {
                                url: url.clone(),
                                consent: true,
                            };
                            probe.validate()?;
                            answers.external_probe = Some(probe);
                        }
                        Answer::Next(_) => answers.external_probe = None,
                        Answer::Back => continue,
                    }
                }
                platform::target()?;
                match choose(terminal, checks_view(&answers), 0)? {
                    Answer::Next(_) => stage = 3,
                    Answer::Back => stage = 1,
                }
            }
            3 => {
                let available = platform::startup_available();
                match choose(
                    terminal,
                    startup_view(available, usize::from(!available)),
                    usize::from(!available),
                )? {
                    Answer::Next(i) => {
                        answers.startup = if i == 0 {
                            Startup::Automatic
                        } else {
                            Startup::Manual
                        };
                        stage = 4;
                    }
                    Answer::Back => stage = 2,
                }
            }
            _ => match choose(terminal, review_view(&answers, 0), 0)? {
                Answer::Next(0) => return Ok(answers),
                _ => stage = 3,
            },
        }
    }
}
pub fn validate_port(text: &str) -> Result<()> {
    let port = text
        .parse::<u16>()
        .ok()
        .filter(|p| *p > 0)
        .ok_or("Enter a port between 1 and 65535.")?;
    if !engine::port_available(port) {
        return Err("That port is already in use. Choose another port or retry.".into());
    }
    Ok(())
}
fn friendly(code: &str) -> String {
    match code{
    "setup_directory_not_empty"=>"Choose an empty, dedicated Vesta folder. Existing Minecraft files belong in the previous step.",
    "setup_existing_server_overlap"=>"Keep Vesta's installation folder separate from the existing Minecraft server.",
    "setup_link_refused"=>"Choose a direct directory path without a link or junction.",
    "setup_path_invalid"=>"Enter a complete directory path without parent components.",
    _=>"This folder is unavailable. Choose another dedicated installation directory.",
}.into()
}
