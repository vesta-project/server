//! Java-compatible text bounds for persisted contracts (lengths count UTF-16 units).
use crate::{Result, StateError};

pub(crate) fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn java_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{001c}'..='\u{0020}' | '\u{1680}' |
        '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200a}' | '\u{2028}'..='\u{2029}' |
        '\u{205f}' | '\u{3000}')
}

pub(crate) fn require_text(value: &str, max: usize) -> Result<()> {
    if value.chars().all(java_whitespace)
        || utf16_len(value) > max
        || value
            .chars()
            .any(|c| matches!(c, '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}'))
    {
        return Err(StateError::new("state_invalid_request"));
    }
    Ok(())
}

pub(crate) fn optional_text(value: Option<&str>, max: usize) -> Result<()> {
    value.map_or(Ok(()), |value| require_text(value, max))
}
