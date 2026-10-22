//! Recognize actual live credential fragments without guessing from ordinary words.
use super::{Hash, ManagementError};
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use std::collections::HashMap;

pub(super) struct CredentialIndex {
    key: hmac::Key,
    counts: HashMap<Hash, usize>,
}

impl CredentialIndex {
    pub(super) fn new() -> Result<Self, ManagementError> {
        let mut key = zeroize::Zeroizing::new([0u8; 32]);
        SystemRandom::new()
            .fill(key.as_mut())
            .map_err(|_| ManagementError::Unavailable)?;
        Ok(Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, key.as_ref()),
            counts: HashMap::new(),
        })
    }

    fn fingerprint(&self, edge: u8, fragment: &str) -> Hash {
        let mut context = hmac::Context::with_key(&self.key);
        context.update(&[edge]);
        context.update(fragment.as_bytes());
        context.sign().as_ref().try_into().expect("SHA-256 length")
    }

    pub(super) fn register(&mut self, secret: &str) -> Box<[Hash]> {
        debug_assert!(secret.is_ascii() && secret.len() == 43);
        let mut fingerprints = Vec::with_capacity(84);
        for length in 1..43 {
            // A leading run can be a suffix; a trailing run can be a prefix.
            for hash in [
                self.fingerprint(b'L', &secret[43 - length..]),
                self.fingerprint(b'R', &secret[..length]),
            ] {
                *self.counts.entry(hash).or_default() += 1;
                fingerprints.push(hash);
            }
        }
        fingerprints.into_boxed_slice()
    }

    pub(super) fn remove(&mut self, fingerprints: &[Hash]) {
        for hash in fingerprints {
            if let Some(count) = self.counts.get_mut(hash) {
                *count -= 1;
                if *count == 0 {
                    self.counts.remove(hash);
                }
            }
        }
    }

    pub(super) fn redact(&self, text: &str) -> String {
        let candidate = |byte: &u8| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_');
        let leading = text.bytes().take_while(candidate).count();
        let trailing = text.bytes().rev().take_while(candidate).count();
        let leading = if (1..43).contains(&leading)
            && self
                .counts
                .contains_key(&self.fingerprint(b'L', &text[..leading]))
        {
            leading
        } else {
            0
        };
        let trailing = if (1..43).contains(&trailing)
            && self
                .counts
                .contains_key(&self.fingerprint(b'R', &text[text.len() - trailing..]))
        {
            trailing
        } else {
            0
        };
        let end = text.len().saturating_sub(trailing).max(leading);
        let mut out = String::with_capacity(text.len() + 20);
        if leading != 0 {
            out.push_str("[redacted]");
        }
        out.push_str(&text[leading..end]);
        if trailing != 0 && (end > leading || leading == 0) {
            out.push_str("[redacted]");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_fragments_are_hidden_without_destroying_minecraft_logs() {
        let mut index = CredentialIndex::new().unwrap();
        let token = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";
        let registration = index.register(token);
        for offset in 1..43 {
            assert_eq!(
                index.redact(&format!("message: {}", &token[..offset])),
                "message: [redacted]"
            );
            assert_eq!(
                index.redact(&format!("{} :message", &token[offset..])),
                "[redacted] :message"
            );
        }
        for text in [
            "Starting minecraft server version 26.3",
            "Loading properties",
            "Default game type: SURVIVAL",
            "Saving players",
            "Starting Minecraft server on 127.0.0.1:25565",
            "WARNING: A restricted method in java.lang.System has been called",
            "Eatham532 left the game",
            "Server empty for 60 seconds, pausing",
            "λ UTF-8 λ",
            "[redacted]",
            "",
        ] {
            assert_eq!(index.redact(text), text);
        }
        // Shared fragments stay registered until the last live credential ends.
        let duplicate = index.register(token);
        index.remove(&registration);
        assert_eq!(index.redact(&token[..21]), "[redacted]");
        index.remove(&duplicate);
        assert!(index.counts.is_empty());
        assert_eq!(index.redact(&token[..21]), &token[..21]);
    }
}
