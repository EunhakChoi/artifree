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
                let line = serde_json::json!({
                    "event": "ready",
                    "api_url": ready.api_url,
                    "content_origin": ready.content_origin,
                    "token_file": ready.token_file,
                    "web_url": ready.web_url,
                });
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
    let mut web_dir = None;
    let mut lan = None;
    let mut api_address = serve::DEFAULT_API_ADDRESS.to_owned();
    let mut content_address = serve::DEFAULT_CONTENT_ADDRESS.to_owned();
    let mut content_origin = None;
    let mut frame_ancestors = serve::DEFAULT_FRAME_ANCESTORS.to_owned();
    let mut explicit_api_listen = false;
    let mut explicit_content_listen = false;
    let mut explicit_frame_ancestors = false;
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
            "--web-dir" => web_dir = Some(PathBuf::from(value()?)),
            "--lan" => {
                let ip_str = value()?;
                let parsed: IpAddr = ip_str
                    .parse()
                    .map_err(|_| "--lan must be an IP address, such as 192.168.1.50.")?;
                lan = Some(parsed);
            }
            "--api-listen" => {
                api_address = value()?;
                explicit_api_listen = true;
            }
            "--content-listen" => {
                content_address = value()?;
                explicit_content_listen = true;
            }
            "--content-origin" => content_origin = Some(value()?),
            "--frame-ancestors" => {
                frame_ancestors = value()?;
                explicit_frame_ancestors = true;
            }
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
    let web_dir = web_dir.or_else(env_web_dir);
    let lan = match lan {
        Some(lan) => Some(lan),
        None => env_lan()?,
    };
    if let Some(ip) = lan {
        let defaults = LanAddresses::from_ip(ip);
        if !explicit_api_listen {
            api_address = defaults.api_address;
        }
        if !explicit_content_listen {
            content_address = defaults.content_address;
            if content_origin.is_none() {
                content_origin = defaults.content_origin;
            }
        }
        if !explicit_frame_ancestors {
            frame_ancestors = defaults.frame_ancestors;
        }
    }
    let address = |flag: &str, value: &str| {
        value
            .parse::<SocketAddr>()
            .map_err(|_| format!("{flag} must be an IP address and port, such as 127.0.0.1:7410."))
    };
    Ok(ServeOptions {
        data_dir: data_dir.ok_or("serve requires --data-dir <directory>.")?,
        web_dir,
        lan,
        api_address: address("--api-listen", &api_address)?,
        content_address: address("--content-listen", &content_address)?,
        content_origin,
        frame_ancestors,
        preview_ttl,
        limits,
        change_streams,
    })
}

fn parse_preview_ttl(raw: &str) -> Result<Duration, String> {
    let range = || {
        format!(
            "--preview-ttl-seconds must be a whole number from 1 to {}.",
            serve::MAX_PREVIEW_TTL.as_secs()
        )
    };
    let seconds: u64 = raw.parse().map_err(|_| range())?;
    let duration = Duration::from_secs(seconds);
    if seconds == 0 || duration > serve::MAX_PREVIEW_TTL {
        return Err(range());
    }
    Ok(duration)
}

fn env_web_dir() -> Option<PathBuf> {
    std::env::var("ARTIFREE_WEB_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
}

fn env_lan() -> Result<Option<IpAddr>, String> {
    let Some(raw) = std::env::var("ARTIFREE_LAN")
        .ok()
        .filter(|s| !s.trim().is_empty())
    else {
        return Ok(None);
    };
    raw.parse()
        .map(Some)
        .map_err(|_| "ARTIFREE_LAN must be an IP address, such as 192.168.1.50.".into())
}

struct LanAddresses {
    api_address: String,
    content_address: String,
    content_origin: Option<String>,
    frame_ancestors: String,
}

impl LanAddresses {
    fn from_ip(ip: IpAddr) -> Self {
        let (api_address, content_address) = match ip {
            IpAddr::V4(v4) => (format!("{v4}:7410"), format!("{v4}:7411")),
            IpAddr::V6(v6) => (format!("[{v6}]:7410"), format!("[{v6}]:7411")),
        };
        let origin = match ip {
            IpAddr::V4(v4) => format!("http://{v4}:7411"),
            IpAddr::V6(v6) => format!("http://[{v6}]:7411"),
        };
        let frame_ancestors = match ip {
            IpAddr::V4(v4) => format!("http://127.0.0.1:* http://localhost:* http://{v4}:*"),
            IpAddr::V6(v6) => format!("http://127.0.0.1:* http://localhost:* http://[{v6}]:*"),
        };
        Self {
            api_address,
            content_address,
            content_origin: Some(origin),
            frame_ancestors,
        }
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
  --web-dir <dir>               Production web assets directory (serves web UI and SPA fallback)
  --lan <ip>                    Serve on local network IP address (sets default listeners, origin, frame-ancestors)
  --api-listen <addr:port>      API address [default: {api}]
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
