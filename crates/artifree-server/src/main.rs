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
use artifree_server::data_backup;
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
        [command, options @ ..] if command == "backup" => {
            let (data_dir, output) = parse_paths(options, "--data-dir", "--output")?;
            let summary = data_backup::backup(&data_dir, &output)?;
            println!(
                "{}",
                serde_json::json!({
                    "event": "backup",
                    "output": output,
                    "schema_version": summary.schema_version,
                    "bytes": summary.bytes,
                })
            );
            Ok(())
        }
        [command, options @ ..] if command == "restore" => {
            let (input, data_dir) = parse_paths(options, "--input", "--data-dir")?;
            let summary = data_backup::restore(&input, &data_dir)?;
            println!(
                "{}",
                serde_json::json!({
                    "event": "restore",
                    "data_dir": data_dir,
                    "schema_version": summary.schema_version,
                    "bytes": summary.bytes,
                })
            );
            Ok(())
        }
        _ => Err("Unknown arguments. Use --help.".into()),
    }
}

/// Parses exactly two required path flags, in any order.
fn parse_paths(
    arguments: &[String],
    first: &str,
    second: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let (mut first_value, mut second_value) = (None, None);
    let mut remaining = arguments.iter();
    while let Some(flag) = remaining.next() {
        let slot = if flag == first {
            &mut first_value
        } else if flag == second {
            &mut second_value
        } else {
            return Err(format!("Unknown arguments. Unexpected {flag}; use --help."));
        };
        let value = remaining
            .next()
            .ok_or_else(|| format!("Unknown arguments. {flag} requires a value; use --help."))?;
        if slot.replace(PathBuf::from(value)).is_some() {
            return Err(format!("Unknown arguments. {flag} was given twice."));
        }
    }
    match (first_value, second_value) {
        (Some(one), Some(two)) => Ok((one, two)),
        _ => Err(format!(
            "Unknown arguments. {first} and {second} are both required; use --help."
        )),
    }
}

struct CliServeFlags {
    data_dir: Option<PathBuf>,
    cli_web_dir: Option<PathBuf>,
    cli_lan: Option<String>,
    explicit_api_listen: Option<String>,
    explicit_loopback_listen: Option<String>,
    explicit_content_listen: Option<String>,
    explicit_content_origin: Option<String>,
    explicit_api_origin: Option<String>,
    cli_trusted_proxies: Vec<String>,
    cli_allowed_hosts: Vec<String>,
    cli_allowed_origins: Vec<String>,
    cli_allow_remote_mcp: bool,
    explicit_frame_ancestors: Option<String>,
    preview_ttl: Duration,
    limits: RequestLimits,
    change_streams: u32,
}

fn parse_serve_flags(arguments: &[String]) -> Result<CliServeFlags, String> {
    let mut flags = CliServeFlags {
        data_dir: None,
        cli_web_dir: None,
        cli_lan: None,
        explicit_api_listen: None,
        explicit_loopback_listen: None,
        explicit_content_listen: None,
        explicit_content_origin: None,
        explicit_api_origin: None,
        cli_trusted_proxies: Vec::new(),
        cli_allowed_hosts: Vec::new(),
        cli_allowed_origins: Vec::new(),
        cli_allow_remote_mcp: false,
        explicit_frame_ancestors: None,
        preview_ttl: serve::DEFAULT_PREVIEW_TTL,
        limits: RequestLimits::default(),
        change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
    };
    let mut remaining = arguments.iter();
    while let Some(flag) = remaining.next() {
        let mut value = || {
            remaining
                .next()
                .cloned()
                .ok_or_else(|| format!("Unknown arguments. {flag} requires a value; use --help."))
        };
        match flag.as_str() {
            "--data-dir" => flags.data_dir = Some(PathBuf::from(value()?)),
            "--web-dir" => flags.cli_web_dir = Some(PathBuf::from(value()?)),
            "--lan" => flags.cli_lan = Some(value()?),
            "--api-listen" => flags.explicit_api_listen = Some(value()?),
            "--loopback-listen" => flags.explicit_loopback_listen = Some(value()?),
            "--content-listen" => flags.explicit_content_listen = Some(value()?),
            "--content-origin" => flags.explicit_content_origin = Some(value()?),
            "--api-origin" => flags.explicit_api_origin = Some(value()?),
            "--trusted-proxy" => flags.cli_trusted_proxies.push(value()?),
            "--allowed-host" => flags.cli_allowed_hosts.push(value()?),
            "--allowed-origin" => flags.cli_allowed_origins.push(value()?),
            "--allow-remote-mcp" => flags.cli_allow_remote_mcp = true,
            "--frame-ancestors" => flags.explicit_frame_ancestors = Some(value()?),
            "--preview-ttl-seconds" => flags.preview_ttl = parse_preview_ttl(&value()?)?,
            "--request-burst" => flags.limits.burst = limit(flag, &value()?)?,
            "--requests-per-minute" => flags.limits.per_minute = limit(flag, &value()?)?,
            "--concurrent-requests" => flags.limits.concurrent = limit(flag, &value()?)?,
            "--change-streams" => flags.change_streams = limit(flag, &value()?)?,
            other => {
                return Err(format!(
                    "Unknown arguments. Unrecognized serve option {other}; use --help."
                ));
            }
        }
    }
    Ok(flags)
}

fn parse_serve(arguments: &[String]) -> Result<ServeOptions, String> {
    let flags = parse_serve_flags(arguments)?;

    let explicit_web_dir = match flags.cli_web_dir {
        Some(dir) => Some(dir),
        None => std::env::var("ARTIFREE_WEB_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
    };

    let lan_ip = resolve_lan_ip(flags.cli_lan)?;
    let (default_api, default_content, default_origin) = resolve_lan_defaults(lan_ip);

    let api_str = flags.explicit_api_listen.unwrap_or(default_api);
    let content_str = flags
        .explicit_content_listen
        .clone()
        .unwrap_or(default_content);
    let resolved_content_origin = resolve_content_origin(
        flags.explicit_content_origin,
        flags.explicit_content_listen.is_some(),
        default_origin,
    )?;
    let resolved_api_origin = resolve_api_origin(flags.explicit_api_origin)?;

    if let (Some(api_orig), Some(content_orig)) = (&resolved_api_origin, &resolved_content_origin)
        && api_orig == content_orig
    {
        return Err(
            "--content-origin must differ from --api-origin to maintain preview sandbox isolation"
                .to_owned(),
        );
    }

    let trusted_proxies = resolve_trusted_proxies(flags.cli_trusted_proxies)?;
    let allowed_hosts = resolve_allowed_hosts(
        flags.cli_allowed_hosts,
        resolved_api_origin.as_deref(),
        lan_ip,
    );
    let allowed_origins =
        resolve_allowed_origins(flags.cli_allowed_origins, resolved_api_origin.as_deref());
    let allow_remote_mcp = flags.cli_allow_remote_mcp
        || std::env::var("ARTIFREE_ALLOW_REMOTE_MCP").is_ok_and(|v| v == "true" || v == "1");

    let frame_ancestors = derive_frame_ancestors(
        flags.explicit_frame_ancestors,
        &api_str,
        resolved_api_origin.as_deref(),
    );

    let address = |flag: &str, value: &str| {
        value
            .parse::<SocketAddr>()
            .map_err(|_| format!("{flag} must be an IP address and port, such as 127.0.0.1:7410."))
    };

    let loopback_api_address = resolve_loopback_listen(flags.explicit_loopback_listen)?;

    Ok(ServeOptions {
        data_dir: flags
            .data_dir
            .ok_or("serve requires --data-dir <directory>.")?,
        web_dir: explicit_web_dir,
        api_address: address("--api-listen", &api_str)?,
        loopback_api_address,
        content_address: address("--content-listen", &content_str)?,
        content_origin: resolved_content_origin,
        api_origin: resolved_api_origin,
        trusted_proxies,
        allowed_hosts,
        allowed_origins,
        allow_remote_mcp,
        frame_ancestors,
        preview_ttl: flags.preview_ttl,
        limits: flags.limits,
        change_streams: flags.change_streams,
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

/// Derives CSP frame-ancestors based on explicit setting, API listener address, or public API origin.
fn derive_frame_ancestors(
    explicit: Option<String>,
    api_address: &str,
    api_origin: Option<&str>,
) -> String {
    if let Some(ancestors) = explicit {
        return ancestors;
    }
    if let Some(origin) = api_origin {
        return format!("{origin} {}", serve::DEFAULT_FRAME_ANCESTORS);
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

/// Resolves public API origin, validating and normalizing explicit origin.
fn resolve_api_origin(explicit: Option<String>) -> Result<Option<String>, String> {
    let raw = match explicit {
        Some(origin) => Some(origin),
        None => std::env::var("ARTIFREE_API_ORIGIN")
            .ok()
            .filter(|s| !s.is_empty()),
    };
    match raw {
        Some(origin) => {
            let canon = artifree_server::http::normalize_origin(&origin).map_err(|e| {
                format!("--api-origin must be a valid HTTP or HTTPS origin, got {origin:?}: {e}")
            })?;
            Ok(Some(canon))
        }
        None => Ok(None),
    }
}

/// Resolves trusted proxy IP/CIDR list from CLI arguments and environment variables.
fn resolve_trusted_proxies(
    cli_proxies: Vec<String>,
) -> Result<Vec<artifree_server::proxy::IpNetwork>, String> {
    let mut result = Vec::new();
    for entry in cli_proxies {
        for part in entry.split(',') {
            let part = part.trim();
            if !part.is_empty() {
                let net: artifree_server::proxy::IpNetwork = part.parse()?;
                result.push(net);
            }
        }
    }
    if let Ok(env_proxies) = std::env::var("ARTIFREE_TRUSTED_PROXIES") {
        for part in env_proxies.split(',') {
            let part = part.trim();
            if !part.is_empty() {
                let net: artifree_server::proxy::IpNetwork = part.parse()?;
                result.push(net);
            }
        }
    }
    Ok(result)
}

/// Resolves allowed hosts, incorporating explicit hosts, environment variables, API origin host, and LAN IP.
fn resolve_allowed_hosts(
    cli_hosts: Vec<String>,
    api_origin: Option<&str>,
    lan_ip: Option<IpAddr>,
) -> Vec<String> {
    let mut result = Vec::new();
    for entry in cli_hosts {
        for part in entry.split(',') {
            let part = part.trim();
            if !part.is_empty() && !result.iter().any(|h: &String| h.eq_ignore_ascii_case(part)) {
                result.push(part.to_owned());
            }
        }
    }
    if let Ok(env_hosts) = std::env::var("ARTIFREE_ALLOWED_HOSTS") {
        for part in env_hosts.split(',') {
            let part = part.trim();
            if !part.is_empty() && !result.iter().any(|h: &String| h.eq_ignore_ascii_case(part)) {
                result.push(part.to_owned());
            }
        }
    }
    if let Some(origin) = api_origin
        && let Some(host) = artifree_server::http::origin_host(origin)
        && !result
            .iter()
            .any(|h: &String| h.eq_ignore_ascii_case(&host))
    {
        result.push(host);
    }
    if let Some(ip) = lan_ip {
        let ip_str = if ip.is_ipv6() {
            format!("[{ip}]")
        } else {
            format!("{ip}")
        };
        let host_port = format!("{ip_str}:7410");
        if !result
            .iter()
            .any(|h: &String| h.eq_ignore_ascii_case(&ip_str))
        {
            result.push(ip_str);
        }
        if !result
            .iter()
            .any(|h: &String| h.eq_ignore_ascii_case(&host_port))
        {
            result.push(host_port);
        }
    }
    for lb in &[
        "localhost",
        "127.0.0.1",
        "[::1]",
        "localhost:7410",
        "127.0.0.1:7410",
        "[::1]:7410",
    ] {
        if !result.iter().any(|h: &String| h.eq_ignore_ascii_case(lb)) {
            result.push((*lb).to_owned());
        }
    }
    result
}

/// Resolves allowed CORS origins, incorporating explicit origins, environment variables, and API origin.
fn resolve_allowed_origins(cli_origins: Vec<String>, api_origin: Option<&str>) -> Vec<String> {
    let mut result = Vec::new();
    for entry in cli_origins {
        for part in entry.split(',') {
            let part = part.trim();
            if !part.is_empty() && !result.contains(&part.to_owned()) {
                result.push(part.to_owned());
            }
        }
    }
    if let Ok(env_origins) = std::env::var("ARTIFREE_ALLOWED_ORIGINS") {
        for part in env_origins.split(',') {
            let part = part.trim();
            if !part.is_empty() && !result.contains(&part.to_owned()) {
                result.push(part.to_owned());
            }
        }
    }
    if let Some(origin) = api_origin
        && !result.contains(&origin.to_owned())
    {
        result.push(origin.to_owned());
    }
    result
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
  artifree-server backup --data-dir <dir> --output <file>
  artifree-server restore --input <file> --data-dir <new-dir>
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
  --api-origin <origin>         Public API and web origin, if proxied (or ARTIFREE_API_ORIGIN)
  --trusted-proxy <cidr>        Trusted reverse proxy IP or CIDR (or ARTIFREE_TRUSTED_PROXIES).
                                The proxy must append (or overwrite) the client
                                address in X-Forwarded-For or Forwarded and strip
                                the header it does not use. A request with no
                                client address, or a chain of only trusted
                                proxies, has an unknown client.
  --allowed-host <host>         Permitted Host header value (or ARTIFREE_ALLOWED_HOSTS)
  --allowed-origin <origin>     Permitted CORS origin (or ARTIFREE_ALLOWED_ORIGINS)
  --allow-remote-mcp            Permit remote authenticated MCP connections. Without
                                it /mcp admits only a known loopback client: connect
                                local agents directly, not through a trusted proxy.
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

backup writes a consistent snapshot of the database to a new file while a
server may be running on the directory, and prints one JSON line on stdout:
{{\"event\":\"backup\",\"output\":...,\"schema_version\":...,\"bytes\":...}}
It refuses an existing --output and creates it readable by its owner only. The
file holds every artifact and the token hashes unencrypted; it does not hold
the access token file.
restore creates a data directory from such a file, upgrading its schema, and
prints {{\"event\":\"restore\",...}}. It refuses a directory that holds
anything, a damaged file, and a schema newer than this build. A new access token
is created when the restored directory first serves.

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

    #[test]
    fn api_origin_and_content_origin_separation() -> Result<(), Box<dyn std::error::Error>> {
        // Differing origins are accepted
        let opts = parse_serve(&args(&[
            "--data-dir",
            "/tmp",
            "--api-origin",
            "https://artifree.example.com",
            "--content-origin",
            "https://preview.example.com",
            "--trusted-proxy",
            "127.0.0.1,10.0.0.0/8",
            "--allow-remote-mcp",
        ]))
        .map_err(|e| format!("valid remote proxy options: {e}"))?;

        assert_eq!(
            opts.api_origin.as_deref(),
            Some("https://artifree.example.com")
        );
        assert_eq!(
            opts.content_origin.as_deref(),
            Some("https://preview.example.com")
        );
        assert!(opts.allow_remote_mcp);
        assert_eq!(opts.trusted_proxies.len(), 2);
        assert!(
            opts.allowed_hosts
                .iter()
                .any(|h| h == "artifree.example.com")
        );

        // Identical origins are strictly rejected to maintain sandbox isolation
        let conflict = parse_serve(&args(&[
            "--data-dir",
            "/tmp",
            "--api-origin",
            "https://artifree.example.com",
            "--content-origin",
            "https://artifree.example.com",
        ]));
        let err = conflict.err().unwrap_or_default();
        assert!(err.contains(
            "--content-origin must differ from --api-origin to maintain preview sandbox isolation"
        ));

        Ok(())
    }
}
