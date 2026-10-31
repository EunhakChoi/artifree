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
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use artifree_server::changes;
use artifree_server::limits::RequestLimits;
use artifree_server::serve::{self, ServeOptions};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
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
                let mut map = serde_json::Map::new();
                map.insert("event".into(), "ready".into());
                map.insert("api_url".into(), ready.api_url.clone().into());
                map.insert("content_origin".into(), ready.content_origin.clone().into());
                map.insert(
                    "token_file".into(),
                    ready.token_file.to_string_lossy().to_string().into(),
                );
                if let Some(ref web_url) = ready.web_url {
                    map.insert("web_url".into(), web_url.clone().into());
                }
                let line = serde_json::Value::Object(map);
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
    let mut explicit_api_listen = None;
    let mut explicit_content_listen = None;
    let mut explicit_content_origin = None;
    let mut explicit_frame_ancestors = None;
    let mut preview_ttl = serve::DEFAULT_PREVIEW_TTL;
    let mut limits = RequestLimits::default();
    let mut change_streams = changes::DEFAULT_STREAMS_PER_ACTOR;
    let mut lan_ip: Option<std::net::IpAddr> = None;
    let mut web_dir = std::env::var_os("ARTIFREE_WEB_DIR").map(PathBuf::from);

    if let Ok(lan_env) = std::env::var("ARTIFREE_LAN") {
        let trimmed = lan_env.trim();
        if !trimmed.is_empty() {
            let ip: std::net::IpAddr = trimmed
                .parse()
                .map_err(|_| "ARTIFREE_LAN must be an IP address such as 192.168.1.100 or ::1.")?;
            lan_ip = Some(ip);
        }
    }

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
            "--api-listen" => explicit_api_listen = Some(value()?),
            "--content-listen" => explicit_content_listen = Some(value()?),
            "--content-origin" => explicit_content_origin = Some(value()?),
            "--frame-ancestors" => explicit_frame_ancestors = Some(value()?),
            "--web-dir" => web_dir = Some(PathBuf::from(value()?)),
            "--lan" => {
                let val = value()?;
                let ip: std::net::IpAddr = val
                    .parse()
                    .map_err(|_| "--lan must be an IP address such as 192.168.1.100 or ::1.")?;
                lan_ip = Some(ip);
            }
            "--preview-ttl-seconds" => {
                let range = || {
                    format!(
                        "--preview-ttl-seconds must be a whole number from 1 to {}.",
                        serve::MAX_PREVIEW_TTL.as_secs()
                    )
                };
                let seconds: u64 = value()?.parse().map_err(|_| range())?;
                preview_ttl = Duration::from_secs(seconds);
                if seconds == 0 || preview_ttl > serve::MAX_PREVIEW_TTL {
                    return Err(range());
                }
            }
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

    let (default_api, default_content, default_origin) = resolve_lan_defaults(lan_ip);

    let api_address = explicit_api_listen.unwrap_or(default_api);
    let content_address = explicit_content_listen
        .as_deref()
        .unwrap_or(&default_content)
        .to_owned();

    let content_origin = if let Some(origin) = explicit_content_origin {
        Some(origin)
    } else if explicit_content_listen.is_none() {
        default_origin
    } else {
        None
    };

    let frame_ancestors =
        explicit_frame_ancestors.unwrap_or_else(|| derive_frame_ancestors(&api_address));

    let address = |flag: &str, value: &str| {
        value
            .parse::<SocketAddr>()
            .map_err(|_| format!("{flag} must be an IP address and port, such as 127.0.0.1:7410."))
    };

    Ok(ServeOptions {
        data_dir: data_dir.ok_or("serve requires --data-dir <directory>.")?,
        api_address: address("--api-listen", &api_address)?,
        content_address: address("--content-listen", &content_address)?,
        content_origin,
        frame_ancestors,
        preview_ttl,
        limits,
        change_streams,
        web_dir,
    })
}

fn resolve_lan_defaults(lan_ip: Option<std::net::IpAddr>) -> (String, String, Option<String>) {
    if let Some(ip) = lan_ip {
        let (api_str, content_str) = match ip {
            std::net::IpAddr::V4(v4) => (format!("{v4}:7410"), format!("{v4}:7411")),
            std::net::IpAddr::V6(v6) => (format!("[{v6}]:7410"), format!("[{v6}]:7411")),
        };
        let origin = format!("http://{content_str}");
        (api_str, content_str, Some(origin))
    } else {
        (
            serve::DEFAULT_API_ADDRESS.to_owned(),
            serve::DEFAULT_CONTENT_ADDRESS.to_owned(),
            None,
        )
    }
}

fn derive_frame_ancestors(api_address: &str) -> String {
    if api_address.starts_with("127.0.0.1:")
        || api_address.starts_with("localhost:")
        || api_address.starts_with("[::1]:")
    {
        serve::DEFAULT_FRAME_ANCESTORS.to_owned()
    } else {
        format!("{} http://{api_address}", serve::DEFAULT_FRAME_ANCESTORS)
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
  --api-listen <addr:port>      API address [default: {api}]
  --content-listen <addr:port>  Preview address [default: {content}]
  --content-origin <origin>     Public preview origin, if proxied
  --frame-ancestors <sources>   CSP frame-ancestors [default: {ancestors}]
  --preview-ttl-seconds <n>     Preview link lifetime, 1-86400 [default: 900]
  --web-dir <dir>               Serve production web assets (env: ARTIFREE_WEB_DIR)
  --lan <ip>                    Bind API and preview to LAN IP (env: ARTIFREE_LAN)
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

    #[test]
    fn derives_frame_ancestors_for_loopback() {
        assert_eq!(
            derive_frame_ancestors("127.0.0.1:7410"),
            serve::DEFAULT_FRAME_ANCESTORS
        );
        assert_eq!(
            derive_frame_ancestors("localhost:7410"),
            serve::DEFAULT_FRAME_ANCESTORS
        );
        assert_eq!(
            derive_frame_ancestors("[::1]:7410"),
            serve::DEFAULT_FRAME_ANCESTORS
        );
    }

    #[test]
    fn derives_frame_ancestors_for_lan_and_custom_ports() {
        assert_eq!(
            derive_frame_ancestors("192.168.1.50:7410"),
            format!(
                "{} http://192.168.1.50:7410",
                serve::DEFAULT_FRAME_ANCESTORS
            )
        );
        assert_eq!(
            derive_frame_ancestors("192.168.1.50:9000"),
            format!(
                "{} http://192.168.1.50:9000",
                serve::DEFAULT_FRAME_ANCESTORS
            )
        );
    }

    #[test]
    fn parse_serve_overrides_frame_ancestors_correctly() -> Result<(), Box<dyn std::error::Error>> {
        let opts = parse_serve(&[
            "--data-dir".into(),
            "/tmp/test-data".into(),
            "--lan".into(),
            "192.168.1.50".into(),
            "--api-listen".into(),
            "192.168.1.50:9000".into(),
        ])?;

        assert_eq!(
            opts.frame_ancestors,
            format!(
                "{} http://192.168.1.50:9000",
                serve::DEFAULT_FRAME_ANCESTORS
            )
        );

        let explicit = parse_serve(&[
            "--data-dir".into(),
            "/tmp/test-data".into(),
            "--lan".into(),
            "192.168.1.50".into(),
            "--frame-ancestors".into(),
            "https://custom.app".into(),
        ])?;

        assert_eq!(explicit.frame_ancestors, "https://custom.app");
        Ok(())
    }
}
