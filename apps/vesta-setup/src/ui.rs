//! Pure terminal rendering for the Vesta setup wizard.
//! The caller owns input, resizing, and redraw behavior.

use std::fmt::Write as _;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Debug)]
pub struct Theme {
    pub color: bool,
    pub unicode: bool,
    pub width: usize,
}

#[derive(Clone, Debug)]
pub struct View {
    pub step: usize,
    pub heading: String,
    pub description: Vec<String>,
    pub rows: Vec<Row>,
    pub hint: String,
}

#[derive(Clone, Debug)]
pub enum Row {
    Choice {
        label: String,
        description: String,
        selected: bool,
        disabled: bool,
    },
    Input {
        label: String,
        value: String,
        error: Option<String>,
    },
    Detail {
        label: String,
        value: String,
    },
    Task {
        label: String,
        status: TaskStatus,
    },
    Notice {
        tone: Tone,
        text: String,
    },
    Empty,
}

#[derive(Clone, Debug)]
pub enum TaskStatus {
    Pending,
    Active,
    Done,
    Failed,
}
#[derive(Clone, Debug)]
pub enum Tone {
    Info,
    Success,
    Warning,
    Error,
}

const STEPS: [&str; 6] = [
    "Welcome", "Location", "Checks", "Startup", "Review", "Finish",
];
const TEAL: &str = "38;2;116;220;207";
const LAVENDER: &str = "38;2;184;172;219";
const MUTED: &str = "38;2;137;146;160";
const TEXT: &str = "38;2;225;229;236";
const GREEN: &str = "38;2;126;211;163";
const AMBER: &str = "38;2;244;193;105";
const RED: &str = "38;2;255;128;137";

#[derive(Clone)]
struct StyledLine {
    text: String,
    style: &'static str,
}
impl StyledLine {
    fn new(text: String, style: &'static str) -> Self {
        Self { text, style }
    }
}

/// Render terminal-safe text with a visible width between 40 and 100 columns.
/// Only generated SGR color sequences are emitted, and only when `theme.color` is true.
pub fn render(view: &View, theme: Theme) -> String {
    let width = theme.width.clamp(40, 100);
    let content_width = width.saturating_sub(4).max(1);
    let step = view.step.clamp(1, STEPS.len());
    let mut lines = Vec::<StyledLine>::new();
    lines.push(StyledLine::new("VESTA  /  SETUP".into(), TEAL));
    lines.push(StyledLine::new(
        format!(
            "STEP {} OF {}  /  {}",
            step,
            STEPS.len(),
            STEPS[step - 1].to_uppercase()
        ),
        LAVENDER,
    ));
    let marks = (1..=STEPS.len())
        .map(|i| {
            if theme.unicode {
                if i < step {
                    '●'
                } else if i == step {
                    '◆'
                } else {
                    '○'
                }
            } else if i < step {
                '#'
            } else if i == step {
                '>'
            } else {
                'o'
            }
        })
        .collect::<String>();
    lines.push(StyledLine::new(marks, MUTED));
    lines.push(StyledLine::new(String::new(), TEXT));

    lines.extend(
        wrap(&sanitize(&view.heading), content_width)
            .into_iter()
            .map(|s| StyledLine::new(s, TEXT_BOLD)),
    );
    for description in view.description.iter().take(20) {
        let clean = sanitize(description);
        if !clean.is_empty() {
            lines.extend(
                wrap(&clean, content_width)
                    .into_iter()
                    .map(|s| StyledLine::new(s, MUTED)),
            );
        }
    }
    if !view.rows.is_empty() {
        lines.push(StyledLine::new(String::new(), TEXT));
    }
    let mut previous: Option<&Row> = None;
    for row in view.rows.iter().take(100) {
        if matches!(row, Row::Empty) {
            section_gap(&mut lines);
            previous = None;
            continue;
        }
        if previous.is_some_and(|previous| {
            !matches!(
                (previous, row),
                (Row::Choice { .. }, Row::Choice { .. })
                    | (Row::Detail { .. }, Row::Detail { .. })
                    | (Row::Task { .. }, Row::Task { .. })
            )
        }) {
            section_gap(&mut lines);
        }
        render_row(row, &theme, content_width, &mut lines);
        previous = Some(row);
    }
    while lines.last().is_some_and(|line| line.text.is_empty()) {
        lines.pop();
    }

    let mut out = String::new();
    for line in lines {
        let rendered = fit(&line.text, content_width, theme.unicode);
        let _ = writeln!(out, "  {}", paint(&rendered, line.style, &theme));
    }
    let hint = sanitize(&view.hint);
    if !hint.is_empty() {
        let _ = writeln!(out);
        for line in wrap(&hint, content_width) {
            let _ = writeln!(out, "  {}", paint(&line, MUTED, &theme));
        }
    }
    out
}

const TEXT_BOLD: &str = "1;38;2;243;244;248";

fn section_gap(lines: &mut Vec<StyledLine>) {
    if lines.last().is_some_and(|line| !line.text.is_empty()) {
        lines.push(StyledLine::new(String::new(), TEXT));
    }
}

fn render_row(row: &Row, theme: &Theme, width: usize, out: &mut Vec<StyledLine>) {
    match row {
        Row::Choice {
            label,
            description,
            selected,
            disabled,
        } => {
            let marker = if *selected {
                if theme.unicode { "›" } else { ">" }
            } else {
                " "
            };
            let suffix = if *disabled { "  (unavailable)" } else { "" };
            let label_style = if *disabled {
                MUTED
            } else if *selected {
                TEAL
            } else {
                TEXT
            };
            for (i, part) in wrap(
                &sanitize(&format!("{label}{suffix}")),
                width.saturating_sub(2),
            )
            .into_iter()
            .enumerate()
            {
                let prefix = if i == 0 { marker } else { " " };
                out.push(StyledLine::new(format!("{prefix} {part}"), label_style));
            }
            let help = sanitize(description);
            if !help.is_empty() {
                out.extend(
                    wrap(&help, width.saturating_sub(4))
                        .into_iter()
                        .map(|s| StyledLine::new(format!("    {}", s), MUTED)),
                );
            }
        }
        Row::Input {
            label,
            value,
            error,
        } => {
            out.extend(
                wrap(&sanitize(label), width)
                    .into_iter()
                    .map(|s| StyledLine::new(s, MUTED)),
            );
            let card_width = width.saturating_sub(3).max(1);
            let (tl, tr, bl, br, h, v) = if theme.unicode {
                ('╭', '╮', '╰', '╯', '─', '│')
            } else {
                ('+', '+', '+', '+', '-', '|')
            };
            out.push(StyledLine::new(
                format!(" {}{}{}", tl, h.to_string().repeat(card_width), tr),
                TEAL,
            ));
            let value = if value.is_empty() {
                "type a path here".to_owned()
            } else {
                sanitize(value)
            };
            for part in wrap(&value, card_width.saturating_sub(2)) {
                let padded = pad_to(&part, card_width.saturating_sub(2));
                out.push(StyledLine::new(format!(" {} {} {}", v, padded, v), TEXT));
            }
            out.push(StyledLine::new(
                format!(" {}{}{}", bl, h.to_string().repeat(card_width), br),
                TEAL,
            ));
            if let Some(error) = error {
                out.extend(
                    wrap(&sanitize(error), width)
                        .into_iter()
                        .map(|s| StyledLine::new(s, RED)),
                );
            }
        }
        Row::Detail { label, value } => {
            let label = sanitize(label);
            let value = sanitize(value);
            let label_width = (width / 3).clamp(8, 20).min(width.saturating_sub(4));
            let value_width = width.saturating_sub(label_width + 2).max(1);
            let values = wrap(&value, value_width);
            for (i, part) in values.iter().enumerate() {
                let prefix = if i == 0 {
                    truncate(&label, label_width, theme.unicode)
                } else {
                    String::new()
                };
                let label_pad = " ".repeat(label_width.saturating_sub(display_width(&prefix)));
                out.push(StyledLine::new(
                    format!("{}{}  {}", prefix, label_pad, part),
                    if i == 0 { TEXT } else { MUTED },
                ));
            }
        }
        Row::Task { label, status } => {
            let (symbol, state, style) = match status {
                TaskStatus::Pending => (if theme.unicode { "○" } else { "o" }, "waiting", MUTED),
                TaskStatus::Active => (if theme.unicode { "◌" } else { "*" }, "in progress", TEAL),
                TaskStatus::Done => (if theme.unicode { "✓" } else { "v" }, "done", GREEN),
                TaskStatus::Failed => (if theme.unicode { "×" } else { "x" }, "failed", RED),
            };
            let suffix = format!("  [{}]", state);
            let text_width = width.saturating_sub(display_width(&suffix) + 1).max(1);
            for part in wrap(&sanitize(label), text_width) {
                out.push(StyledLine::new(
                    format!("{} {}{}", symbol, part, suffix),
                    style,
                ));
            }
        }
        Row::Notice { tone, text } => {
            let (tag, style) = match tone {
                Tone::Info => ("INFO", TEAL),
                Tone::Success => ("NOTE", GREEN),
                Tone::Warning => ("CHECK", AMBER),
                Tone::Error => ("ISSUE", RED),
            };
            let prefix = format!("{}  ", tag);
            let prefix_width = display_width(&prefix);
            let parts = wrap(&sanitize(text), width.saturating_sub(prefix_width).max(1));
            for (i, part) in parts.iter().enumerate() {
                let text = if i == 0 {
                    format!("{}{}", prefix, part)
                } else {
                    format!("{}{}", " ".repeat(prefix_width), part)
                };
                out.push(StyledLine::new(text, style));
            }
        }
        Row::Empty => out.push(StyledLine::new(String::new(), TEXT)),
    }
}

fn paint(text: &str, style: &str, theme: &Theme) -> String {
    if theme.color {
        format!("\x1b[{}m{}\x1b[0m", style, text)
    } else {
        text.to_owned()
    }
}

fn sanitize(s: &str) -> String {
    // Bound each untrusted value before whitespace normalization or wrapping.
    s.chars()
        .take(4096)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if s.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        let mut part = String::new();
        for c in word.chars() {
            let cw = UnicodeWidthChar::width(c).unwrap_or(0);
            if display_width(&part) + cw > width {
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                if !part.is_empty() {
                    lines.push(std::mem::take(&mut part));
                }
            }
            part.push(c);
        }
        if !part.is_empty() {
            let sep = if line.is_empty() { 0 } else { 1 };
            if display_width(&line) + sep + display_width(&part) > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&part);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn fit(s: &str, width: usize, unicode: bool) -> String {
    pad_to(&truncate(s, width, unicode), width)
}
fn pad_to(s: &str, width: usize) -> String {
    format!(
        "{}{}",
        s,
        " ".repeat(width.saturating_sub(display_width(s)))
    )
}

fn truncate(s: &str, width: usize, unicode: bool) -> String {
    if display_width(s) <= width {
        return s.to_owned();
    }
    let marker = if unicode { "…" } else { "..." };
    if width < display_width(marker) {
        return " ".repeat(width);
    }
    let budget = width - display_width(marker);
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cw > budget {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push_str(marker);
    out
}
