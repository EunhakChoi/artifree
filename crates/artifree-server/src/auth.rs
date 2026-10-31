//! Local access token. One token maps to the single local owner actor; this
//! is not multi-user authentication.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use artifree_domain::identity::{ActorId, TokenId};

const TOKEN_BYTES: usize = 32;
const AGENT_PREFIX: &str = "art_";

/// Authenticated caller context.
/// Context for an authenticated caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthContext {
    /// The actor identity for this request (the single owner).
    pub(crate) actor: ActorId,
    /// The token ID if an agent token was used, or None for the bootstrap owner token.
    pub(crate) token_id: Option<TokenId>,
    /// Whether the caller has administrative privileges.
    pub(crate) is_admin: bool,
}

/// A secret bearer token. `Debug` never prints the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessToken(..)")
    }
}

impl AccessToken {
    /// Generates a bootstrap owner token from system randomness (64 lowercase hex digits).
    ///
    /// # Errors
    ///
    /// When the operating system provides no randomness.
    pub fn generate() -> io::Result<Self> {
        let mut bytes = [0; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Self(hex(&bytes)))
    }

    /// Generates a prefixed agent token from system randomness (`art_` followed by 64 hex digits).
    ///
    /// # Errors
    ///
    /// When the operating system provides no randomness.
    pub fn generate_agent_token() -> io::Result<Self> {
        let mut bytes = [0; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        let mut text = String::with_capacity(AGENT_PREFIX.len() + TOKEN_BYTES * 2);
        text.push_str(AGENT_PREFIX);
        text.push_str(&hex(&bytes));
        Ok(Self(text))
    }

    /// Parses a token: either 64 lowercase hexadecimal digits (bootstrap format)
    /// or `art_` followed by 64 lowercase hexadecimal digits (agent format).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let hex_part = value.strip_prefix(AGENT_PREFIX).unwrap_or(value);
        (hex_part.len() == TOKEN_BYTES * 2
            && hex_part
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then(|| Self(value.to_owned()))
    }

    /// Masked prefix string suitable for identification without exposing the secret.
    #[must_use]
    pub fn prefix(&self) -> String {
        masked_prefix(&self.0)
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

/// Generates a masked prefix for a token secret string.
#[must_use]
pub fn masked_prefix(secret: &str) -> String {
    if let Some(rest) = secret.strip_prefix(AGENT_PREFIX) {
        let visible = &rest[..rest.len().min(8)];
        format!("{AGENT_PREFIX}{visible}...")
    } else {
        let visible = &secret[..secret.len().min(8)];
        format!("{visible}...")
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

    #[test]
    fn agent_token_generation_parsing_and_prefix() -> Result<(), Box<dyn std::error::Error>> {
        let agent = AccessToken::generate_agent_token()?;
        assert!(agent.secret().starts_with("art_"));
        assert_eq!(agent.secret().len(), 4 + 64);
        assert_eq!(AccessToken::parse(agent.secret()), Some(agent.clone()));
        assert!(agent.prefix().starts_with("art_"));
        assert!(agent.prefix().ends_with("..."));

        let legacy = AccessToken::generate()?;
        assert_eq!(legacy.secret().len(), 64);
        assert_eq!(AccessToken::parse(legacy.secret()), Some(legacy.clone()));
        assert_eq!(legacy.prefix().len(), 11);
        Ok(())
    }
}
