//! Java Properties logical lines and decoded keys; duplicate keys use the last value.
use std::collections::BTreeMap;
fn decode(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let h: String = chars.by_ref().take(4).collect();
                out.push(
                    u32::from_str_radix(&h, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .unwrap_or('\u{fffd}'),
                );
            }
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}
pub(super) fn parse(text: &str) -> BTreeMap<String, String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    for part in normalized.lines() {
        let part = part.trim_start_matches([' ', '\t', '\u{c}']);
        if line.is_empty() && (part.starts_with(['#', '!']) || part.is_empty()) {
            continue;
        }
        line.push_str(part);
        if line.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1 {
            line.pop();
        } else {
            lines.push(std::mem::take(&mut line));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    let mut out = BTreeMap::new();
    for line in lines {
        let mut escaped = false;
        let split = line
            .char_indices()
            .find_map(|(i, c)| {
                if escaped {
                    escaped = false;
                    return None;
                }
                if c == '\\' {
                    escaped = true;
                    return None;
                }
                matches!(c, '=' | ':' | ' ' | '\t' | '\u{c}').then_some(i)
            })
            .unwrap_or(line.len());
        let value = line[split..].trim_start_matches([' ', '\t', '\u{c}']);
        let value = value
            .strip_prefix(['=', ':'])
            .unwrap_or(value)
            .trim_start_matches([' ', '\t', '\u{c}']);
        out.insert(decode(&line[..split]), decode(value));
    }
    out
}
fn encode(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' | '=' | ':' | ' ' | '#' | '!' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}
pub(super) fn rewrite(text: &str, port: u16) -> String {
    let mut values = parse(text);
    for (k, v) in [
        ("enable-rcon", "false".into()),
        ("enable-query", "false".into()),
        ("server-port", port.to_string()),
        ("query.port", port.to_string()),
        ("server-ip", String::new()),
    ] {
        values.insert(k.into(), v);
    }
    values
        .into_iter()
        .map(|(k, v)| format!("{}={}\n", encode(&k), encode(&v)))
        .collect()
}
pub(super) fn safe_world(v: &str) -> bool {
    !v.is_empty()
        && !v.contains(['\\', ':'])
        && !v.starts_with('/')
        && v.split('/').all(|p| !matches!(p, "" | "." | ".."))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hostile_properties() {
        let text = "!comment\nenable-rcon true\nenable\\u002drcon:true\nserver\\-port\\\n =123\nserver-port 456\nserver-ip : 1.2.3.4\n";
        let result = parse(&rewrite(text, 25566));
        assert_eq!(result["enable-rcon"], "false");
        assert_eq!(result["server-port"], "25566");
        assert_eq!(result["server-ip"], "");
        assert_eq!(parse("x=a\\\n  b")["x"], "ab");
        for bad in ["../world", "/world", "C:\\world", "a/../../b"] {
            assert!(!safe_world(bad));
        }
    }
}
