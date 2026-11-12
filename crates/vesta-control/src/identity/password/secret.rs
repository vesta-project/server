use super::PasswordError;
use std::fmt;
use zeroize::Zeroizing;

/// Rust-native bounds: bytes and Unicode scalars, not Java UTF-16 code units.
pub const MAX_PASSWORD_BYTES: usize = 4096;
pub const MAX_PASSWORD_SCALARS: usize = 1024;

/// Owns a human password and erases the owned buffer on every exit, including failed
/// validation and unwinding. Construction consumes the caller's allocation, without
/// trimming, normalization, truncation, cloning or an intermediate secret String.
///
/// This cannot erase copies the caller made earlier, compiler temporaries, swap or
/// crash dumps, and does not establish a protected-process or transport boundary.
pub struct SecretPassword {
    bytes: Zeroizing<Vec<u8>>,
}

impl TryFrom<Vec<u8>> for SecretPassword {
    type Error = PasswordError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        // Install the drop guard BEFORE any validation failure can return.
        let bytes = Zeroizing::new(bytes);
        if bytes.is_empty() || bytes.len() > MAX_PASSWORD_BYTES {
            return Err(PasswordError::InvalidPassword);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| PasswordError::InvalidPassword)?;
        if text.chars().take(MAX_PASSWORD_SCALARS + 1).count() > MAX_PASSWORD_SCALARS {
            return Err(PasswordError::InvalidPassword);
        }
        Ok(Self { bytes })
    }
}

impl TryFrom<String> for SecretPassword {
    type Error = PasswordError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::try_from(text.into_bytes())
    }
}

impl SecretPassword {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for SecretPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretPassword(<redacted>)")
    }
}

/// An owned PHC intended only for private persistence. Not a possession proof, grant,
/// serializable response or public verifier accessor. Its owned text is wiped on drop.
pub struct PasswordVerifier {
    phc: Zeroizing<String>,
}

impl PasswordVerifier {
    pub(super) fn new(phc: String) -> Self {
        Self {
            phc: Zeroizing::new(phc),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.phc
    }
}

impl fmt::Debug for PasswordVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PasswordVerifier(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumes_vec_and_string_without_changing_password_bytes() {
        let input = "  exact\0password-café  ".as_bytes().to_vec();
        let owned = SecretPassword::try_from(input.clone()).unwrap();
        assert_eq!(owned.as_bytes(), input);
        let string = SecretPassword::try_from("  exact\0password-café  ".to_owned()).unwrap();
        assert_eq!(owned.as_bytes(), string.as_bytes());
        assert_eq!(format!("{owned:?}"), "SecretPassword(<redacted>)");
    }

    #[test]
    fn byte_and_scalar_bounds_are_independent_and_inclusive() {
        let maximum = "\u{1f510}".repeat(MAX_PASSWORD_SCALARS);
        assert_eq!(maximum.len(), MAX_PASSWORD_BYTES);
        assert!(SecretPassword::try_from(maximum).is_ok());
        assert!(SecretPassword::try_from("漢".repeat(MAX_PASSWORD_SCALARS)).is_ok());
        assert_eq!(
            SecretPassword::try_from(vec![b'x'; MAX_PASSWORD_SCALARS + 1]).unwrap_err(),
            PasswordError::InvalidPassword
        );
        assert_eq!(
            SecretPassword::try_from(vec![b'x'; MAX_PASSWORD_BYTES + 1]).unwrap_err(),
            PasswordError::InvalidPassword
        );
    }

    #[test]
    fn empty_and_malformed_utf8_are_fixed_material_free_errors() {
        for input in [Vec::new(), vec![0xff], vec![0xed, 0xa0, 0x80]] {
            let error = SecretPassword::try_from(input).unwrap_err();
            assert_eq!(error, PasswordError::InvalidPassword);
            assert_eq!(error.to_string(), "Password operation failed");
            assert!(std::error::Error::source(&error).is_none());
        }
        let verifier = PasswordVerifier::new("private verifier fixture".to_owned());
        assert_eq!(format!("{verifier:?}"), "PasswordVerifier(<redacted>)");
    }
}
