//! Proxy and network security: trusted reverse proxy evaluation, Host/Origin
//! validation, and remote client identification.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use artifree_contracts::api::ErrorCode;
use axum::http::{HeaderMap, StatusCode, header};

use crate::operations::ApiError;

/// An IP network represented by an address and CIDR prefix length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpNetwork {
    /// IPv4 network with prefix length 0..=32.
    V4 {
        /// Base IPv4 address.
        addr: Ipv4Addr,
        /// Prefix length in bits.
        prefix: u8,
    },
    /// IPv6 network with prefix length 0..=128.
    V6 {
        /// Base IPv6 address.
        addr: Ipv6Addr,
        /// Prefix length in bits.
        prefix: u8,
    },
}

impl IpNetwork {
    /// Returns whether the network contains the given IP address.
    #[must_use]
    pub fn contains(&self, ip: &IpAddr) -> bool {
        match (self, ip) {
            (Self::V4 { addr, prefix }, IpAddr::V4(target)) => {
                if *prefix == 0 {
                    return true;
                }
                let mask = if *prefix >= 32 {
                    u32::MAX
                } else {
                    !((1u32 << (32 - prefix)) - 1)
                };
                (u32::from(*addr) & mask) == (u32::from(*target) & mask)
            }
            (Self::V6 { addr, prefix }, IpAddr::V6(target)) => {
                if *prefix == 0 {
                    return true;
                }
                let mask = if *prefix >= 128 {
                    u128::MAX
                } else {
                    !((1u128 << (128 - prefix)) - 1)
                };
                (u128::from(*addr) & mask) == (u128::from(*target) & mask)
            }
            _ => false,
        }
    }
}

impl FromStr for IpNetwork {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if let Some((ip_str, prefix_str)) = trimmed.split_once('/') {
            let ip: IpAddr = ip_str
                .parse()
                .map_err(|e| format!("invalid IP address in CIDR {trimmed:?}: {e}"))?;
            let prefix: u8 = prefix_str
                .parse()
                .map_err(|e| format!("invalid prefix length in CIDR {trimmed:?}: {e}"))?;
            match ip {
                IpAddr::V4(addr) => {
                    if prefix > 32 {
                        return Err(format!("IPv4 prefix length must be <= 32 in {trimmed:?}"));
                    }
                    Ok(Self::V4 { addr, prefix })
                }
                IpAddr::V6(addr) => {
                    if prefix > 128 {
                        return Err(format!("IPv6 prefix length must be <= 128 in {trimmed:?}"));
                    }
                    Ok(Self::V6 { addr, prefix })
                }
            }
        } else {
            let unbracketed = trimmed
                .strip_prefix('[')
                .and_then(|t| t.strip_suffix(']'))
                .unwrap_or(trimmed);
            let ip: IpAddr = unbracketed
                .parse()
                .map_err(|e| format!("invalid IP address or CIDR {trimmed:?}: {e}"))?;
            match ip {
                IpAddr::V4(addr) => Ok(Self::V4 { addr, prefix: 32 }),
                IpAddr::V6(addr) => Ok(Self::V6 { addr, prefix: 128 }),
            }
        }
    }
}

/// Evaluated client connection metadata after proxy inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConnectionInfo {
    /// Effective client IP address (forwarded by trusted proxy or direct peer).
    pub client_ip: IpAddr,
    /// Effective host header (X-Forwarded-Host if trusted proxy, else Host).
    pub host: String,
    /// Effective protocol scheme ("http" or "https").
    pub proto: String,
    /// Whether the request arrived via an acknowledged trusted reverse proxy.
    pub is_proxied: bool,
}

/// Evaluates request connection headers against trusted reverse proxy rules.
///
/// # Errors
///
/// Returns `ApiError` when an untrusted peer submits forwarded headers (spoofing prevention),
/// or when forwarded headers are syntactically invalid.
pub(crate) fn evaluate_connection(
    headers: &HeaderMap,
    peer_ip: IpAddr,
    trusted_proxies: &[IpNetwork],
) -> Result<ClientConnectionInfo, ApiError> {
    let is_trusted_proxy = trusted_proxies.iter().any(|net| net.contains(&peer_ip));

    let has_forwarded_headers = headers.contains_key("x-forwarded-for")
        || headers.contains_key("x-forwarded-host")
        || headers.contains_key("x-forwarded-proto")
        || headers.contains_key("forwarded");

    if !is_trusted_proxy {
        if has_forwarded_headers {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                ErrorCode::InvalidRequest,
                "forwarded headers from untrusted peer are forbidden",
            ));
        }
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map_or("", str::trim)
            .to_owned();
        return Ok(ClientConnectionInfo {
            client_ip: peer_ip,
            host,
            proto: "http".to_owned(),
            is_proxied: false,
        });
    }

    let client_ip = parse_trusted_client_ip(headers, peer_ip)?;
    let host = parse_trusted_host(headers)?;
    let proto = parse_trusted_proto(headers);

    Ok(ClientConnectionInfo {
        client_ip,
        host,
        proto,
        is_proxied: true,
    })
}

fn parse_trusted_client_ip(headers: &HeaderMap, peer_ip: IpAddr) -> Result<IpAddr, ApiError> {
    if let Some(xff) = headers.get("x-forwarded-for") {
        let xff_str = xff.to_str().map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidRequest,
                "x-forwarded-for header is not valid ASCII",
            )
        })?;
        let first_entry = xff_str.split(',').next().map_or("", str::trim);
        let unbracketed = first_entry
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(first_entry);
        unbracketed.parse::<IpAddr>().map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidRequest,
                "x-forwarded-for header contains an invalid IP address",
            )
        })
    } else if let Some(fwd) = headers.get("forwarded") {
        let fwd_str = fwd.to_str().map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidRequest,
                "forwarded header is not valid ASCII",
            )
        })?;
        extract_forwarded_for(fwd_str).ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidRequest,
                "forwarded header does not specify a valid for parameter",
            )
        })
    } else {
        Ok(peer_ip)
    }
}

fn parse_trusted_host(headers: &HeaderMap) -> Result<String, ApiError> {
    if let Some(xfh) = headers.get("x-forwarded-host") {
        let host_str = xfh.to_str().map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidRequest,
                "x-forwarded-host header is not valid ASCII",
            )
        })?;
        Ok(host_str.trim().to_owned())
    } else if let Some(fwd) = headers.get("forwarded") {
        let fallback = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map_or("", str::trim)
            .to_owned();
        Ok(fwd
            .to_str()
            .ok()
            .and_then(extract_forwarded_host)
            .unwrap_or(fallback))
    } else {
        Ok(headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map_or("", str::trim)
            .to_owned())
    }
}

fn parse_trusted_proto(headers: &HeaderMap) -> String {
    if let Some(xfp) = headers.get("x-forwarded-proto") {
        xfp.to_str().unwrap_or("http").trim().to_ascii_lowercase()
    } else if let Some(fwd) = headers.get("forwarded") {
        fwd.to_str()
            .ok()
            .and_then(extract_forwarded_proto)
            .unwrap_or_else(|| "http".to_owned())
    } else {
        "http".to_owned()
    }
}

/// Extracts the client IP from a standard RFC 7239 `Forwarded` header string.
fn extract_forwarded_for(header_value: &str) -> Option<IpAddr> {
    for part in header_value.split(';') {
        let trimmed = part.trim();
        if let Some(rest) = trimmed.strip_prefix("for=") {
            let val = rest.trim_matches('"');
            let unbracketed = val
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(val);
            if let Ok(ip) = unbracketed.parse::<IpAddr>() {
                return Some(ip);
            }
        }
    }
    None
}

/// Extracts the host from a standard RFC 7239 `Forwarded` header string.
fn extract_forwarded_host(header_value: &str) -> Option<String> {
    for part in header_value.split(';') {
        let trimmed = part.trim();
        if let Some(rest) = trimmed.strip_prefix("host=") {
            let val = rest.trim_matches('"').trim();
            if !val.is_empty() {
                return Some(val.to_owned());
            }
        }
    }
    None
}

/// Extracts the protocol from a standard RFC 7239 `Forwarded` header string.
fn extract_forwarded_proto(header_value: &str) -> Option<String> {
    for part in header_value.split(';') {
        let trimmed = part.trim();
        if let Some(rest) = trimmed.strip_prefix("proto=") {
            let val = rest.trim_matches('"').trim().to_ascii_lowercase();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Checks whether an effective host is permitted under configured allowed hosts.
#[must_use]
pub fn is_host_allowed(host: &str, allowed_hosts: &[String]) -> bool {
    if allowed_hosts.is_empty() {
        return true;
    }
    let incoming = normalize_host(host);
    if allowed_hosts
        .iter()
        .any(|allowed| normalize_host(allowed) == incoming)
    {
        return true;
    }

    let incoming_name = host_name(&incoming);
    for allowed in allowed_hosts {
        let normalized = normalize_host(allowed);
        if let Some(prefix) = normalized.strip_suffix(":*")
            && prefix.eq_ignore_ascii_case(incoming_name)
        {
            return true;
        }
        if is_loopback_host(&normalized) && normalized.eq_ignore_ascii_case(incoming_name) {
            return true;
        }
    }

    false
}

fn host_name(host: &str) -> &str {
    if let Some(bracket_end) = host.find(']') {
        &host[..=bracket_end]
    } else if let Some(colon) = host.find(':') {
        &host[..colon]
    } else {
        host
    }
}

fn is_loopback_host(host: &str) -> bool {
    let name = host_name(host);
    name.eq_ignore_ascii_case("localhost")
        || name == "127.0.0.1"
        || name == "[::1]"
        || name == "::1"
}

/// Normalizes a host string (lowercase, strips redundant default ports :80 and :443).
#[must_use]
pub fn normalize_host(host: &str) -> String {
    let lower = host.trim().to_ascii_lowercase();
    if let Some(stripped) = lower.strip_suffix(":80") {
        stripped.to_owned()
    } else if let Some(stripped) = lower.strip_suffix(":443") {
        stripped.to_owned()
    } else {
        lower
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_network_parsing_and_matching() -> Result<(), String> {
        let net4: IpNetwork = "192.168.1.0/24".parse()?;
        assert!(net4.contains(&"192.168.1.1".parse().map_err(|e| format!("{e}"))?));
        assert!(net4.contains(&"192.168.1.254".parse().map_err(|e| format!("{e}"))?));
        assert!(!net4.contains(&"192.168.2.1".parse().map_err(|e| format!("{e}"))?));

        let ipv4_single: IpNetwork = "127.0.0.1".parse()?;
        assert!(ipv4_single.contains(&"127.0.0.1".parse().map_err(|e| format!("{e}"))?));
        assert!(!ipv4_single.contains(&"127.0.0.2".parse().map_err(|e| format!("{e}"))?));

        let net6: IpNetwork = "2001:db8::/32".parse()?;
        assert!(net6.contains(&"2001:db8::1".parse().map_err(|e| format!("{e}"))?));
        assert!(net6.contains(&"2001:db8:ffff::1".parse().map_err(|e| format!("{e}"))?));
        assert!(!net6.contains(&"2001:db9::1".parse().map_err(|e| format!("{e}"))?));

        let ipv6_single: IpNetwork = "::1".parse()?;
        assert!(ipv6_single.contains(&"::1".parse().map_err(|e| format!("{e}"))?));
        assert!(!ipv6_single.contains(&"::2".parse().map_err(|e| format!("{e}"))?));

        let net_zero: IpNetwork = "0.0.0.0/0".parse()?;
        assert!(net_zero.contains(&"8.8.8.8".parse().map_err(|e| format!("{e}"))?));

        Ok(())
    }

    #[test]
    fn untrusted_peer_forwarded_headers_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.195".parse()?);

        let untrusted_peer: IpAddr = "198.51.100.1".parse()?;
        let trusted = vec!["127.0.0.1".parse::<IpNetwork>()?];

        let result = evaluate_connection(&headers, untrusted_peer, &trusted);
        match result {
            Ok(_) => panic!("expected forbidden error, got success"),
            Err(err) => assert_eq!(err.status, StatusCode::FORBIDDEN),
        }
        Ok(())
    }

    #[test]
    fn trusted_proxy_forwarded_headers_extracted() -> Result<(), Box<dyn std::error::Error>> {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.195, 10.0.0.1".parse()?);
        headers.insert("x-forwarded-host", "artifree.example.com:8443".parse()?);
        headers.insert("x-forwarded-proto", "https".parse()?);

        let proxy_peer = "127.0.0.1".parse::<IpAddr>()?;
        let trusted = vec!["127.0.0.1".parse::<IpNetwork>()?];

        let info =
            evaluate_connection(&headers, proxy_peer, &trusted).map_err(|e| format!("{e:?}"))?;
        assert_eq!(info.client_ip, "203.0.113.195".parse::<IpAddr>()?);
        assert_eq!(info.host, "artifree.example.com:8443");
        assert_eq!(info.proto, "https");
        assert!(info.is_proxied);

        Ok(())
    }

    #[test]
    fn host_normalization_and_allowlist() {
        let allowed = vec![
            "artifree.example.com".to_owned(),
            "localhost:7410".to_owned(),
        ];

        assert!(is_host_allowed("artifree.example.com", &allowed));
        assert!(is_host_allowed("ARTIFREE.EXAMPLE.COM", &allowed));
        assert!(is_host_allowed("artifree.example.com:443", &allowed));
        assert!(is_host_allowed("localhost:7410", &allowed));
        assert!(!is_host_allowed("evil.example.com", &allowed));
        assert!(!is_host_allowed("artifree.example.com:8443", &allowed));
    }
}
