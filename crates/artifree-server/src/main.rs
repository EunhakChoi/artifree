//! `artifree-server`: serve the local API and preview origin, or inspect the
//! build inventory.
#![expect(
    clippy::print_stdout,
    reason = "Help, version, JSON, and the readiness line are the CLI protocol."
)]
#![expect(
    clippy::print_stderr,
    reason = "Command failures must be reported on stderr."
)]

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use artifree_server::changes;
use artifree_server::limits::RequestLimits;
use artifree_server::serve::{self, ServeOptions};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            print_help();
            Ok(())
        }
        [arg] if arg == "--help" || arg == "-h" => {
            print_help();
            Ok(())
        }
        [arg] if arg == "--version" => {
            println!("artifree-server {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        [arg] if arg == "--describe" => {
            let json = serde_json::to_string_pretty(&artifree_server::describe())
                .map_err(|error| format!("Could not serialize the module inventory: {error}"))?;
            println!("{json}");
            Ok(())
        }
        [command, options @ ..] if command == "serve" => {
            let options = parse_serve(options)?;
            serve::run(options, |ready| {
                let mut line = serde_json::json!({
                    "event": "ready",
                    "api_url": ready.api_url,
                    "content_origin": ready.content_origin,
                    "token_file": ready.token_file,
                });
                if let Some(ref web_url) = ready.web_url {
                    line["web_url"] = serde_json::json!(web_url);
                }
                if let Some(ref loopback_url) = ready.loopback_api_url {
                    line["loopback_api_url"] = serde_json::json!(loopback_url);
                }
                println!("{line}");
                // A supervisor waits for this line; a failed flush only delays it.
                if std::io::stdout().flush().is_err() {
                    eprintln!("Could not flush the readiness line.");
                }
            })
        }
        _ => Err("Unknown arguments. Use --help.".into()),
    }
}

fn parse_serve(arguments: &[String]) -> Result<ServeOptions, String> {
    let mut data_dir = None;
    let mut cli_web_dir = None;
    let mut cli_lan = None;
    let mut explicit_api_listen = None;
    let mut explicit_loopback_listen = None;
    let mut explicit_content_listen = None;
    let mut explicit_content_origin = None;
    let mut explicit_frame_ancestors = None;
    let mut preview_ttl = serve::DEFAULT_PREVIEW_TTL;
    let mut limits = RequestLimits::default();
    let mut change_streams = changes::DEFAULT_STREAMS_PER_ACTOR;
    let mut remaining = arguments.iter();
    while let Some(flag) = remaining.next() {
        let mut value = || {
            remaining
                .next()
                .cloned()
                .ok_or_else(|| format!("Unknown arguments. {flag} requires a value; use --help."))
        };
        match flag.as_str() {
            "--data-dir" => data_dir = Some(PathBuf::from(value()?)),
            "--web-dir" => cli_web_dir = Some(PathBuf::from(value()?)),
            "--lan" => cli_lan = Some(value()?),
            "--api-listen" => explicit_api_listen = Some(value()?),
            "--loopback-listen" => explicit_loopback_listen = Some(value()?),
            "--content-listen" => explicit_content_listen = Some(value()?),
            "--content-origin" => explicit_content_origin = Some(value()?),
            "--frame-ancestors" => explicit_frame_ancestors = Some(value()?),
            "--preview-ttl-seconds" => preview_ttl = parse_preview_ttl(&value()?)?,
            "--request-burst" => limits.burst = limit(flag, &value()?)?,
            "--requests-per-minute" => limits.per_minute = limit(flag, &value()?)?,
            "--concurrent-requests" => limits.concurrent = limit(flag, &value()?)?,
            "--change-streams" => change_streams = limit(flag, &value()?)?,
            other => {
                return Err(format!(
                    "Unknown arguments. Unrecognized serve option {other}; use --help."
                ));
            }
        }
    }

    let explicit_web_dir = match cli_web_dir {
        Some(dir) => Some(dir),
        None => std::env::var("ARTIFREE_WEB_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
    };

    let lan_ip = resolve_lan_ip(cli_lan)?;
    let (default_api, default_content, default_origin) = resolve_lan_defaults(lan_ip);

    let api_str = explicit_api_listen.unwrap_or(default_api);
    let content_str = explicit_content_listen.clone().unwrap_or(default_content);
    let resolved_content_origin = resolve_content_origin(
        explicit_content_origin,
        explicit_content_listen.is_some(),
        default_origin,
    )?;

    let frame_ancestors = derive_frame_ancestors(explicit_frame_ancestors, &api_str);

    let address = |flag: &str, value: &str| {
        value
            .parse::<SocketAddr>()
            .map_err(|_| format!("{flag} must be an IP address and port, such as 127.0.0.1:7410."))
    };

    let loopback_api_address = resolve_loopback_listen(explicit_loopback_listen)?;

    Ok(ServeOptions {
        data_dir: data_dir.ok_or("serve requires --data-dir <directory>.")?,
        web_dir: explicit_web_dir,
        api_address: address("--api-listen", &api_str)?,
        loopback_api_address,
        content_address: address("--content-listen", &content_str)?,
        content_origin: resolved_content_origin,
        frame_ancestors,
        preview_ttl,
        limits,
        change_streams,
    })
}

/// Parses the preview link lifetime from a CLI argument.
fn parse_preview_ttl(value: &str) -> Result<Duration, String> {
    let range = || {
        format!(
            "--preview-ttl-seconds must be a whole number from 1 to {}.",
            serve::MAX_PREVIEW_TTL.as_secs()
        )
    };
    let seconds: u64 = value.parse().map_err(|_| range())?;
    let ttl = Duration::from_secs(seconds);
    if seconds == 0 || ttl > serve::MAX_PREVIEW_TTL {
        return Err(range());
    }
    Ok(ttl)
}

/// Resolves an optional LAN IP address from CLI flag or environment variable.
fn resolve_lan_ip(cli_lan: Option<String>) -> Result<Option<IpAddr>, String> {
    let explicit_lan = match cli_lan {
        Some(lan) => Some(lan),
        None => std::env::var("ARTIFREE_LAN").ok().filter(|s| !s.is_empty()),
    };
    match explicit_lan {
        Some(val) => {
            let unbracketed = val
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(&val);
            let ip: IpAddr = unbracketed
                .parse()
                .map_err(|_| format!("--lan must be an IP address (IPv4 or IPv6), got {val:?}"))?;
            if ip.is_unspecified() {
                return Err(format!(
                    "--lan must be a specific IP address (IPv4 or IPv6), not an unspecified address ({val}). Use --api-listen and --content-listen to bind to wildcard addresses."
                ));
            }
            Ok(Some(ip))
        }
        None => Ok(None),
    }
}

/// Resolves preview content origin, validating and normalizing explicit origin.
fn resolve_content_origin(
    explicit: Option<String>,
    explicit_content_listen: bool,
    default_origin: Option<String>,
) -> Result<Option<String>, String> {
    match explicit {
        Some(origin) => {
            let canon = artifree_server::http::normalize_origin(&origin).map_err(|e| {
                format!(
                    "--content-origin must be a valid HTTP or HTTPS origin, got {origin:?}: {e}"
                )
            })?;
            Ok(Some(canon))
        }
        None => {
            if explicit_content_listen {
                Ok(None)
            } else {
                Ok(default_origin)
            }
        }
    }
}

/// Resolves an optional loopback listener for MCP peer isolation in LAN mode.
fn resolve_loopback_listen(explicit: Option<String>) -> Result<Option<SocketAddr>, String> {
    let raw = match explicit {
        Some(addr) => Some(addr),
        None => std::env::var("ARTIFREE_LOOPBACK_LISTEN")
            .ok()
            .filter(|s| !s.is_empty()),
    };
    match raw {
        Some(val) => {
            let addr = val.parse::<SocketAddr>().map_err(|_| {
                "--loopback-listen must be an IP address and port, such as 127.0.0.1:7410."
                    .to_owned()
            })?;
            if !addr.ip().is_loopback() {
                return Err(
                    "--loopback-listen must be a loopback IP address, such as 127.0.0.1 or ::1."
                        .to_owned(),
                );
            }
            Ok(Some(addr))
        }
        None => Ok(None),
    }
}

/// Resolves default listen addresses and preview origin for a given optional LAN IP.
fn resolve_lan_defaults(lan_ip: Option<IpAddr>) -> (String, String, Option<String>) {
    match lan_ip {
        Some(ip) => {
            let formatted_ip = if ip.is_ipv6() {
                format!("[{ip}]")
            } else {
                format!("{ip}")
            };
            (
                format!("{formatted_ip}:7410"),
                format!("{formatted_ip}:7411"),
                Some(format!("http://{formatted_ip}:7411")),
            )
        }
        None => (
            serve::DEFAULT_API_ADDRESS.to_owned(),
            serve::DEFAULT_CONTENT_ADDRESS.to_owned(),
            None,
        ),
    }
}

/// Derives CSP frame-ancestors based on explicit setting or the API listener address.
fn derive_frame_ancestors(explicit: Option<String>, api_address: &str) -> String {
    if let Some(ancestors) = explicit {
        return ancestors;
    }
    let Ok(addr) = api_address.parse::<SocketAddr>() else {
        return serve::DEFAULT_FRAME_ANCESTORS.to_owned();
    };
    let ip = addr.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        serve::DEFAULT_FRAME_ANCESTORS.to_owned()
    } else {
        let formatted_ip = if ip.is_ipv6() {
            format!("[{ip}]")
        } else {
            format!("{ip}")
        };
        format!("http://{formatted_ip}:* {}", serve::DEFAULT_FRAME_ANCESTORS)
    }
}

/// A request limit: a whole number from 1 to 100000.
fn limit(flag: &str, value: &str) -> Result<u32, String> {
    match value.parse::<u32>() {
        Ok(number @ 1..=100_000) => Ok(number),
        _ => Err(format!("{flag} must be a whole number from 1 to 100000.")),
    }
}

fn print_help() {
    println!(
        "Artifree server

Usage:
  artifree-server serve --data-dir <dir> [options]
  artifree-server --describe | --version | --help

serve runs the HTTP API and a separate preview content origin until Ctrl+C or
SIGTERM. When both listeners are ready it prints one JSON line on stdout:
{{\"event\":\"ready\",\"api_url\":...,\"content_origin\":...,\"token_file\":...}}
Clients authenticate with the bearer token stored in token_file.

Options:
  --data-dir <dir>              SQLite database and access token (required).
                                On Unix it is created with mode 700, and a
                                directory other users can access is refused.
  --web-dir <dir>               Directory holding production web assets (or ARTIFREE_WEB_DIR)
  --lan <ip>                    Bind listeners to explicit LAN IP (or ARTIFREE_LAN)
  --api-listen <addr:port>      API address [default: {api}]
  --loopback-listen <addr:port> Dedicated loopback API address for MCP in LAN mode
  --content-listen <addr:port>  Preview address [default: {content}]
  --content-origin <origin>     Public preview origin, if proxied
  --frame-ancestors <sources>   CSP frame-ancestors [default: {ancestors}]
  --preview-ttl-seconds <n>     Preview link lifetime, 1-86400 [default: 900]
  --request-burst <n>           Requests accepted at once [default: {burst}]
  --requests-per-minute <n>     Sustained request rate [default: {rate}]
  --concurrent-requests <n>     Requests running at once [default: {concurrent}]
  --change-streams <n>          Open change streams [default: {streams}]

Request limits apply per actor to HTTP and MCP together; a refused request
gets 429 rate_limited with Retry-After and is not processed. Opening a change
stream counts as one request; an open stream holds one of the actor's
change stream slots instead of a running request.

--describe emits the build-time module inventory as JSON; it does not probe a
running server.",
        api = serve::DEFAULT_API_ADDRESS,
        content = serve::DEFAULT_CONTENT_ADDRESS,
        ancestors = serve::DEFAULT_FRAME_ANCESTORS,
        burst = RequestLimits::default().burst,
        rate = RequestLimits::default().per_minute,
        concurrent = RequestLimits::default().concurrent,
        streams = changes::DEFAULT_STREAMS_PER_ACTOR,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn unspecified_lan_is_rejected() {
        let res_v4 = parse_serve(&args(&["--data-dir", "/tmp", "--lan", "0.0.0.0"]));
        let err_v4 = res_v4.err().unwrap_or_default();
        assert!(err_v4.contains("not an unspecified address (0.0.0.0)"));

        let res_v6 = parse_serve(&args(&["--data-dir", "/tmp", "--lan", "::"]));
        let err_v6 = res_v6.err().unwrap_or_default();
        assert!(err_v6.contains("not an unspecified address (::)"));

        let res_v6_bracket = parse_serve(&args(&["--data-dir", "/tmp", "--lan", "[::]"]));
        let err_v6_bracket = res_v6_bracket.err().unwrap_or_default();
        assert!(err_v6_bracket.contains("not an unspecified address ([::])"));
    }

    #[test]
    fn bracketed_and_unbracketed_lan_are_accepted() -> Result<(), Box<dyn std::error::Error>> {
        let opts_bracket_v6 = parse_serve(&args(&["--data-dir", "/tmp", "--lan", "[::1]"]))
            .map_err(|e| format!("bracketed loopback: {e}"))?;
        assert_eq!(opts_bracket_v6.api_address.to_string(), "[::1]:7410");
        assert_eq!(
            opts_bracket_v6.content_origin.as_deref(),
            Some("http://[::1]:7411")
        );

        let opts_bracket_lan =
            parse_serve(&args(&["--data-dir", "/tmp", "--lan", "[2001:db8::1]"]))
                .map_err(|e| format!("bracketed LAN: {e}"))?;
        assert_eq!(
            opts_bracket_lan.api_address.to_string(),
            "[2001:db8::1]:7410"
        );
        assert_eq!(
            opts_bracket_lan.content_origin.as_deref(),
            Some("http://[2001:db8::1]:7411")
        );

        let opts_v4 = parse_serve(&args(&["--data-dir", "/tmp", "--lan", "192.168.1.50"]))
            .map_err(|e| format!("LAN IPv4: {e}"))?;
        assert_eq!(opts_v4.api_address.to_string(), "192.168.1.50:7410");
        assert_eq!(
            opts_v4.content_origin.as_deref(),
            Some("http://192.168.1.50:7411")
        );
        Ok(())
    }

    #[test]
    fn content_origin_validation_and_normalization() -> Result<(), Box<dyn std::error::Error>> {
        let opts = parse_serve(&args(&[
            "--data-dir",
            "/tmp",
            "--content-origin",
            "http://[0:0:0:0:0:0:0:1]:80/",
        ]))
        .map_err(|e| format!("expanded IPv6 origin: {e}"))?;
        assert_eq!(opts.content_origin.as_deref(), Some("http://[::1]"));

        let res = parse_serve(&args(&[
            "--data-dir",
            "/tmp",
            "--content-origin",
            "not-a-valid-url",
        ]));
        let err = res.err().unwrap_or_default();
        assert!(err.contains("--content-origin must be a valid HTTP or HTTPS origin"));
        Ok(())
    }
}
