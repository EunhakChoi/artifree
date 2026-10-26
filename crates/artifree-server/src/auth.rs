//! Local access token. One token maps to the single local owner actor; this
//! is not multi-user authentication.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

const TOKEN_BYTES: usize = 32;

/// A secret bearer token. `Debug` never prints the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessToken(..)")
    }
}

impl AccessToken {
    /// Generates a token from system randomness.
    ///
    /// # Errors
    ///
    /// When the operating system provides no randomness.
    pub fn generate() -> io::Result<Self> {
        let mut bytes = [0; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Self(hex(&bytes)))
    }

    /// Parses a token of 64 lowercase hexadecimal digits.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        (value.len() == TOKEN_BYTES * 2
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then(|| Self(value.to_owned()))
    }

    /// Reads the token at `path`, or creates one readable only by the current
    /// user (on Unix) when the file does not exist.
    ///
    /// # Errors
    ///
    /// When the file cannot be read or written, or holds an invalid token.
    pub fn load_or_create(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(content) => Self::parse(content.trim()).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} does not contain a valid access token", path.display()),
                )
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let token = Self::generate()?;
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
                let mut file = options.open(path)?;
                writeln!(file, "{}", token.0)?;
                file.sync_all()?;
                Ok(token)
            }
            Err(error) => Err(error),
        }
    }

    /// The secret value, for clients that need to send it.
    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }

    /// Compares in time independent of where the first difference occurs.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        let expected = self.0.as_bytes();
        let candidate = candidate.as_bytes();
        if expected.len() != candidate.len() {
            return false;
        }
        expected
            .iter()
            .zip(candidate)
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            == 0
    }
}

/// Lowercase hexadecimal encoding.
#[must_use]
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_the_exact_token() {
        let token = AccessToken(hex(&[0x5a; TOKEN_BYTES]));
        assert!(token.matches(&"5a".repeat(TOKEN_BYTES)));
        assert!(!token.matches(&format!("{}5b", "5a".repeat(TOKEN_BYTES - 1))));
        assert!(!token.matches(&"5a".repeat(TOKEN_BYTES - 1)));
        assert!(!token.matches(""));
    }

    #[test]
    fn debug_output_hides_the_secret() {
        let token = AccessToken(hex(&[0x5a; TOKEN_BYTES]));
        assert!(!format!("{token:?}").contains("5a5a"));
    }
}
