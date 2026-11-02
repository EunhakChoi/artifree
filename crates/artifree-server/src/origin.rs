//! One strict parser for the http and https origins an operator configures
//! (decision 0028).
//!
//! The parser is written out instead of leaning on a lenient URI parser: such
//! a parser accepts an out-of-range or non-numeric port and reports no port,
//! which would turn `https://ui.example:65536` into the default-port origin.
//! Nothing is canonicalized until the whole text has been validated, and an
//! error never contains any part of the text.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

/// Why an origin was refused. The text of the origin is never part of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginError {
    /// The text is empty.
    Empty,
    /// There is no `scheme://` or the scheme is neither http nor https.
    Scheme,
    /// The origin carries `user:password@` credentials.
    Userinfo,
    /// The host is empty, not a valid name, IPv4 address, or bracketed IPv6.
    Host,
    /// The text after `:` is not a number from 1 to 65535.
    Port,
    /// A path (other than one trailing `/`), query, or fragment follows.
    Trailing,
    /// The text has whitespace, control characters, or non-ASCII characters.
    Characters,
}

impl OriginError {
    /// A short static reason that is safe to log.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Empty => "it is empty",
            Self::Scheme => "it must start with http:// or https://",
            Self::Userinfo => "it must not contain a user name or password",
            Self::Host => "its host must be a name, an IPv4 address, or a bracketed IPv6 address",
            Self::Port => "its port must be digits from 1 to 65535",
            Self::Trailing => "it must not have a path, query, or fragment",
            Self::Characters => "it must be plain ASCII without spaces",
        }
    }
}

impl fmt::Display for OriginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason())
    }
}

impl std::error::Error for OriginError {}

/// Validates `raw` as `http(s)://host[:port]` (with at most one trailing `/`)
/// and returns the canonical origin: lowercase scheme and host, the compressed
/// form of an IPv6 address, and no port when it is the scheme's default.
///
/// # Errors
///
/// An [`OriginError`] naming the first rule the text breaks.
pub fn normalize_origin(raw: &str) -> Result<String, OriginError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(OriginError::Empty);
    }
    if !text.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(OriginError::Characters);
    }
    let (scheme, rest) = text.split_once("://").ok_or(OriginError::Scheme)?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return Err(OriginError::Scheme),
    };
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.contains('@') {
        return Err(OriginError::Userinfo);
    }
    if authority.contains(['/', '?', '#', '\\']) {
        return Err(OriginError::Trailing);
    }
    let (host, port_text) = split_host_port(authority)?;
    let host = canonical_host(host)?;
    let port = match port_text {
        Some(text) => parse_port(text)?,
        None => default_port,
    };
    if port == default_port {
        Ok(format!("{scheme}://{host}"))
    } else {
        Ok(format!("{scheme}://{host}:{port}"))
    }
}

/// Splits `host[:port]`; the port text is `Some` whenever a `:` follows the
/// host, even if nothing follows it.
fn split_host_port(authority: &str) -> Result<(&str, Option<&str>), OriginError> {
    if authority.starts_with('[') {
        let end = authority.find(']').ok_or(OriginError::Host)?;
        let host = &authority[..=end];
        return match &authority[end + 1..] {
            "" => Ok((host, None)),
            after => after
                .strip_prefix(':')
                .map(|port| (host, Some(port)))
                .ok_or(OriginError::Host),
        };
    }
    match authority.split_once(':') {
        Some((host, port)) => Ok((host, Some(port))),
        None => Ok((authority, None)),
    }
}

fn parse_port(text: &str) -> Result<u16, OriginError> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OriginError::Port);
    }
    match text.parse::<u16>() {
        Ok(port @ 1..) => Ok(port),
        _ => Err(OriginError::Port),
    }
}

fn canonical_host(host: &str) -> Result<String, OriginError> {
    if let Some(inner) = host.strip_prefix('[') {
        let inner = inner.strip_suffix(']').ok_or(OriginError::Host)?;
        let address: Ipv6Addr = inner.parse().map_err(|_| OriginError::Host)?;
        return Ok(format!("[{address}]"));
    }
    if host.is_empty() || host.len() > 253 {
        return Err(OriginError::Host);
    }
    // Digits and dots only is an IPv4 address or nothing: `1.2.3` and
    // `999.1.1.1` are not host names.
    if host
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        let address: Ipv4Addr = host.parse().map_err(|_| OriginError::Host)?;
        return Ok(address.to_string());
    }
    let is_name = host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    });
    if is_name {
        Ok(host.to_ascii_lowercase())
    } else {
        Err(OriginError::Host)
    }
}

/// The `host[:port]` part of a valid origin, in canonical form.
#[must_use]
pub fn origin_host(origin: &str) -> Option<String> {
    let canonical = normalize_origin(origin).ok()?;
    canonical
        .split_once("://")
        .map(|(_, authority)| authority.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_forms() {
        for (input, expected) in [
            ("http://example.com", "http://example.com"),
            ("HTTPS://Example.COM", "https://example.com"),
            ("https://ui.example:443", "https://ui.example"),
            ("http://ui.example:80/", "http://ui.example"),
            ("https://ui.example:80", "https://ui.example:80"),
            ("http://ui.example:443", "http://ui.example:443"),
            ("http://127.0.0.1:7410", "http://127.0.0.1:7410"),
            ("  http://localhost:08080  ", "http://localhost:8080"),
            ("http://artifree_web:7410", "http://artifree_web:7410"),
            ("http://[::1]:80/", "http://[::1]"),
            ("http://[0:0:0:0:0:0:0:1]:7411", "http://[::1]:7411"),
            ("https://[2001:DB8::1]:443", "https://[2001:db8::1]"),
            ("http://a.b-c.example:65535", "http://a.b-c.example:65535"),
        ] {
            assert_eq!(
                normalize_origin(input).as_deref(),
                Ok(expected),
                "input {input}"
            );
        }
    }

    #[test]
    fn an_explicit_port_is_never_dropped_silently() {
        for input in [
            "https://ui.example:65536",
            "https://ui.example:0",
            "https://ui.example:abc",
            "https://ui.example:",
            "https://ui.example:-1",
            "https://ui.example:08080x",
            "https://ui.example:+443",
            "https://ui.example:99999999999999999999",
            "https://ui.example:443:443",
            "http://[::1]:65536",
            "http://[::1]:",
            "http://[::1]:x",
        ] {
            let result = normalize_origin(input);
            assert!(
                matches!(result, Err(OriginError::Port | OriginError::Host)),
                "input {input} gave {result:?}"
            );
        }
        assert_eq!(
            normalize_origin("https://ui.example:65536"),
            Err(OriginError::Port)
        );
        assert_eq!(
            normalize_origin("http://[::1]:65536"),
            Err(OriginError::Port)
        );
    }

    #[test]
    fn other_shapes_are_refused_with_their_reason() {
        for (input, expected) in [
            ("", OriginError::Empty),
            ("   ", OriginError::Empty),
            ("example.com", OriginError::Scheme),
            ("//example.com", OriginError::Scheme),
            ("ftp://example.com", OriginError::Scheme),
            ("file:///etc/passwd", OriginError::Scheme),
            ("data:text/html,x", OriginError::Scheme),
            ("https//user:p%40ss@host", OriginError::Scheme),
            ("https://user@example.com", OriginError::Userinfo),
            (
                "https://user:secret@example.com:8080",
                OriginError::Userinfo,
            ),
            ("https://:@example.com", OriginError::Userinfo),
            ("https://", OriginError::Host),
            ("https://:8080", OriginError::Host),
            ("https://-", OriginError::Host),
            ("https://exa mple.com", OriginError::Characters),
            ("https://exämple.com", OriginError::Characters),
            ("https://a..b", OriginError::Host),
            ("https://example.com.", OriginError::Host),
            ("https://999.1.1.1", OriginError::Host),
            ("https://1.2.3", OriginError::Host),
            ("https://01.2.3.4", OriginError::Host),
            ("https://[not-an-ip]", OriginError::Host),
            ("https://[::1", OriginError::Host),
            ("https://[::1]x", OriginError::Host),
            ("https://example.com/path", OriginError::Trailing),
            ("https://example.com//", OriginError::Trailing),
            ("https://example.com?x=1", OriginError::Trailing),
            ("https://example.com?", OriginError::Trailing),
            ("https://example.com#frag", OriginError::Trailing),
            ("https://example.com#", OriginError::Trailing),
            ("https://example.com\\x", OriginError::Trailing),
        ] {
            assert_eq!(normalize_origin(input), Err(expected), "input {input:?}");
        }
    }

    #[test]
    fn no_error_quotes_any_part_of_the_input() {
        for input in [
            "ftp://user:hunter2@host",
            "https://user:hunter2@host:99999",
            "https://user:hunter2@",
            "hunter2",
            "https://[hunter2]",
            "https://hunter2.example:hunter2",
            "https://hunter2.example/hunter2",
        ] {
            let Err(error) = normalize_origin(input) else {
                panic!("input {input} must be refused");
            };
            assert!(
                !format!("{error} {error:?}").contains("hunter2"),
                "input {input}: {error}"
            );
        }
    }

    #[test]
    fn origin_host_keeps_the_canonical_authority() {
        assert_eq!(
            origin_host("https://API.example:8443/").as_deref(),
            Some("api.example:8443")
        );
        assert_eq!(origin_host("http://[::1]:80").as_deref(), Some("[::1]"));
        assert_eq!(origin_host("https://api.example:99999"), None);
    }
}
