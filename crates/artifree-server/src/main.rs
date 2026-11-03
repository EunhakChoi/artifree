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

use std::ffi::OsString;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use artifree_server::changes;
use artifree_server::limits::RequestLimits;
use artifree_server::origin::normalize_origin;
use artifree_server::preview::normalize_frame_ancestors;
use artifree_server::proxy::IpNetwork;
use artifree_server::serve::{self, DataLocation, OneShot, Ready, ServeOptions};

mod maintenance;
mod ready_line;
mod serve_env;
mod worker_command;

use serve_env::{Given, ServeEnv};

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
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // Paths may be any bytes the platform allows, so these three commands read
    // their arguments as they are, as does `worker`.
    if let Some((command, rest)) = args.split_first() {
        if command == "backup" {
            return maintenance::backup_command(rest);
        }
        if command == "restore" {
            return maintenance::restore_command(rest);
        }
        if command == "worker" {
            return worker_command::worker_command(rest);
        }
    }
    let args = args
        .into_iter()
        .map(|argument| {
            // The argument is not shown: it may hold a credential.
            argument
                .into_string()
                .map_err(|_| "Unknown arguments. An argument is not valid UTF-8.".to_owned())
        })
        .collect::<Result<Vec<String>, String>>()?;
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
            let options = parse_serve(options, &ServeEnv::from_process())?;
            let one_shot = matches!(options.data, DataLocation::OneShot(_));
            serve::run(options, |ready| {
                ready_line::write(&mut std::io::stdout().lock(), ready)?;
                if one_shot {
                    tell_one_shot(ready);
                }
                Ok(())
            })?;
            if one_shot {
                // Reached only when the data directory was removed.
                writeln!(
                    std::io::stderr(),
                    "The one-shot session ended and its data directory was deleted."
                )
                .ok();
            }
            Ok(())
        }
        _ => Err("Unknown arguments. Use --help.".into()),
    }
}

/// Tells a person on stderr what a one-shot session keeps (nothing).
fn tell_one_shot(ready: &Ready) {
    let ttl = ready
        .session
        .as_ref()
        .map_or(0, |session| session.ttl.as_secs());
    writeln!(
        std::io::stderr(),
        "One-shot session: nothing is kept. {} and everything in it, including the access token, is deleted when this process ends: after {ttl} seconds in total (start-up time included), on SIGINT or SIGTERM, or on a start-up failure. A killed process leaves it for the next one-shot start to sweep. A tunnel or proxy lifetime is a separate setting that Artifree does not implement.",
        ready.data_dir.display()
    )
    .ok();
}

struct CliServeFlags {
    data_dir: Option<PathBuf>,
    one_shot_ttl: Option<Duration>,
    session_root: Option<PathBuf>,
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
        one_shot_ttl: None,
        session_root: None,
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
            "--one-shot-ttl-seconds" => flags.one_shot_ttl = Some(parse_one_shot_ttl(&value()?)?),
            "--session-root" => flags.session_root = Some(PathBuf::from(value()?)),
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
            other => return Err(unrecognized_option(other)),
        }
    }
    Ok(flags)
}

/// The error for an argument that is not a serve option. Only text that looks
/// like an option name is shown, and only the part before any `=`: the rest
/// may hold a value such as a credential.
fn unrecognized_option(argument: &str) -> String {
    let name = argument.split_once('=').map_or(argument, |(name, _)| name);
    let looks_like_option = name.strip_prefix("--").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    });
    if looks_like_option {
        format!(
            "Unknown arguments. Unrecognized serve option {name}; give an option's value as the next argument and see --help."
        )
    } else {
        "Unknown arguments. An argument is not a serve option; see --help.".to_owned()
    }
}

/// Reads `serve` options. Errors name the flag or variable in effect and
/// never repeat an operator-supplied value (decision 0028).
fn parse_serve(arguments: &[String], env: &ServeEnv) -> Result<ServeOptions, String> {
    let flags = parse_serve_flags(arguments)?;

    let explicit_web_dir = match flags.cli_web_dir {
        Some(dir) => Some(dir),
        None => env.text(serve_env::WEB_DIR).map(PathBuf::from),
    };

    let lan_ip = resolve_lan_ip(env, flags.cli_lan.as_deref())?;
    let (default_api, default_content, default_origin) = resolve_lan_defaults(lan_ip);

    let api_str = flags.explicit_api_listen.unwrap_or(default_api);
    let content_str = flags
        .explicit_content_listen
        .clone()
        .unwrap_or(default_content);
    let resolved_content_origin = resolve_content_origin(
        env,
        flags.explicit_content_origin.as_deref(),
        flags.explicit_content_listen.is_some(),
        default_origin,
    )?;
    let resolved_api_origin = resolve_api_origin(env, flags.explicit_api_origin.as_deref())?;

    if let (Some(api_orig), Some(content_orig)) = (&resolved_api_origin, &resolved_content_origin)
        && api_orig == content_orig
    {
        return Err(
            "--content-origin must differ from --api-origin to maintain preview sandbox isolation (also when they are set by ARTIFREE_CONTENT_ORIGIN and ARTIFREE_API_ORIGIN)"
                .to_owned(),
        );
    }

    let trusted_proxies = resolve_trusted_proxies(env, &flags.cli_trusted_proxies)?;
    let allowed_hosts = resolve_allowed_hosts(
        env,
        &flags.cli_allowed_hosts,
        resolved_api_origin.as_deref(),
        lan_ip,
    );
    let allowed_origins = resolve_allowed_origins(
        env,
        &flags.cli_allowed_origins,
        resolved_api_origin.as_deref(),
    )?;
    let allow_remote_mcp = flags.cli_allow_remote_mcp
        || env
            .text(serve_env::ALLOW_REMOTE_MCP)
            .is_some_and(|v| v == "true" || v == "1");

    let frame_ancestors = resolve_frame_ancestors(
        env,
        flags.explicit_frame_ancestors.as_deref(),
        &api_str,
        resolved_api_origin.as_deref(),
    )?;

    let address = |flag: &str, value: &str| {
        value
            .parse::<SocketAddr>()
            .map_err(|_| format!("{flag} must be an IP address and port, such as 127.0.0.1:7410."))
    };

    let loopback_api_address =
        resolve_loopback_listen(env, flags.explicit_loopback_listen.as_deref())?;

    Ok(ServeOptions {
        data: data_location(flags.data_dir, flags.one_shot_ttl, flags.session_root)?,
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

/// Chooses persistent or one-shot storage. The two exclude each other and a
/// one-shot session needs an explicit lifetime: there is no default, so the
/// mode exists only when asked for (decision 0039).
fn data_location(
    data_dir: Option<PathBuf>,
    one_shot_ttl: Option<Duration>,
    session_root: Option<PathBuf>,
) -> Result<DataLocation, String> {
    match (data_dir, one_shot_ttl, session_root) {
        (Some(_), Some(_), _) => Err(
            "--data-dir and --one-shot-ttl-seconds cannot be combined: a one-shot session keeps its data in a new temporary directory and deletes it, and --data-dir names a directory that is kept."
                .to_owned(),
        ),
        (Some(data_dir), None, None) => Ok(DataLocation::Persistent { data_dir }),
        (Some(_), None, Some(_)) => Err(
            "--session-root only applies with --one-shot-ttl-seconds; a --data-dir is used where it is."
                .to_owned(),
        ),
        (None, Some(ttl), session_root) => Ok(DataLocation::OneShot(OneShot { ttl, session_root })),
        (None, None, Some(_)) => {
            Err("--session-root only applies with --one-shot-ttl-seconds.".to_owned())
        }
        (None, None, None) => Err(
            "serve requires --data-dir <directory> (kept), or --one-shot-ttl-seconds <n> for a temporary session that is deleted."
                .to_owned(),
        ),
    }
}

/// Parses the one-shot session lifetime from a CLI argument.
fn parse_one_shot_ttl(value: &str) -> Result<Duration, String> {
    let range = || {
        format!(
            "--one-shot-ttl-seconds must be a whole number from 1 to {}.",
            serve::MAX_ONE_SHOT_TTL.as_secs()
        )
    };
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(range());
    }
    let seconds: u64 = value.parse().map_err(|_| range())?;
    let ttl = Duration::from_secs(seconds);
    if seconds == 0 || ttl > serve::MAX_ONE_SHOT_TTL {
        return Err(range());
    }
    Ok(ttl)
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
fn resolve_lan_ip(env: &ServeEnv, cli_lan: Option<&str>) -> Result<Option<IpAddr>, String> {
    let Some(given) = env.lenient("--lan", cli_lan, serve_env::LAN) else {
        return Ok(None);
    };
    let unbracketed = given
        .text
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(given.text);
    let ip: IpAddr = unbracketed
        .parse()
        .map_err(|_| format!("{} must be an IP address (IPv4 or IPv6).", given.name))?;
    if ip.is_unspecified() {
        return Err(format!(
            "{} must be a specific IP address (IPv4 or IPv6), not an unspecified address. Use --api-listen and --content-listen to bind to wildcard addresses.",
            given.name
        ));
    }
    Ok(Some(ip))
}

/// Validates an origin setting and returns its canonical form.
fn origin_setting(given: Given<'_>) -> Result<String, String> {
    normalize_origin(given.text).map_err(|error| {
        format!(
            "{} must be a valid HTTP or HTTPS origin: {error}.",
            given.name
        )
    })
}

/// Resolves preview content origin, validating and normalizing explicit origin.
/// The flag wins over `ARTIFREE_CONTENT_ORIGIN`; an empty variable is unset.
fn resolve_content_origin(
    env: &ServeEnv,
    flag: Option<&str>,
    explicit_content_listen: bool,
    default_origin: Option<String>,
) -> Result<Option<String>, String> {
    match env.strict("--content-origin", flag, serve_env::CONTENT_ORIGIN)? {
        Some(given) => origin_setting(given).map(Some),
        None if explicit_content_listen => Ok(None),
        None => Ok(default_origin),
    }
}

/// Resolves an optional loopback listener for MCP peer isolation in LAN mode.
fn resolve_loopback_listen(
    env: &ServeEnv,
    flag: Option<&str>,
) -> Result<Option<SocketAddr>, String> {
    let Some(given) = env.lenient("--loopback-listen", flag, serve_env::LOOPBACK_LISTEN) else {
        return Ok(None);
    };
    let addr = given.text.parse::<SocketAddr>().map_err(|_| {
        format!(
            "{} must be an IP address and port, such as 127.0.0.1:7410.",
            given.name
        )
    })?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "{} must be a loopback IP address, such as 127.0.0.1 or ::1.",
            given.name
        ));
    }
    Ok(Some(addr))
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

/// Resolves CSP `frame-ancestors`: the flag, else `ARTIFREE_FRAME_ANCESTORS`,
/// else a default derived from the API listener or public origin.
fn resolve_frame_ancestors(
    env: &ServeEnv,
    flag: Option<&str>,
    api_address: &str,
    api_origin: Option<&str>,
) -> Result<String, String> {
    match env.strict("--frame-ancestors", flag, serve_env::FRAME_ANCESTORS)? {
        Some(given) => normalize_frame_ancestors(given.text).map_err(|reason| {
            format!(
                "{} must be a space-separated frame-ancestors source list: {reason}.",
                given.name
            )
        }),
        None => Ok(derive_frame_ancestors(api_address, api_origin)),
    }
}

/// Derives CSP frame-ancestors from the API listener address or public API origin.
fn derive_frame_ancestors(api_address: &str, api_origin: Option<&str>) -> String {
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
fn resolve_api_origin(env: &ServeEnv, flag: Option<&str>) -> Result<Option<String>, String> {
    env.lenient("--api-origin", flag, serve_env::API_ORIGIN)
        .map(origin_setting)
        .transpose()
}

/// Resolves trusted proxy IP/CIDR list from CLI arguments and environment variables.
fn resolve_trusted_proxies(env: &ServeEnv, flags: &[String]) -> Result<Vec<IpNetwork>, String> {
    env.entries("--trusted-proxy", flags, serve_env::TRUSTED_PROXIES)
        .into_iter()
        .map(|given| {
            given.text.parse::<IpNetwork>().map_err(|reason| {
                format!(
                    "{} entries must be an IP address or CIDR network: {reason}.",
                    given.name
                )
            })
        })
        .collect()
}

/// Resolves allowed hosts, incorporating explicit hosts, environment variables, API origin host, and LAN IP.
fn resolve_allowed_hosts(
    env: &ServeEnv,
    flags: &[String],
    api_origin: Option<&str>,
    lan_ip: Option<IpAddr>,
) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut add = |host: String| {
        if !result.iter().any(|known| known.eq_ignore_ascii_case(&host)) {
            result.push(host);
        }
    };
    for given in env.entries("--allowed-host", flags, serve_env::ALLOWED_HOSTS) {
        add(given.text.to_owned());
    }
    if let Some(host) = api_origin.and_then(artifree_server::origin::origin_host) {
        add(host);
    }
    if let Some(ip) = lan_ip {
        let ip_str = if ip.is_ipv6() {
            format!("[{ip}]")
        } else {
            format!("{ip}")
        };
        add(ip_str.clone());
        add(format!("{ip_str}:7410"));
    }
    for loopback in [
        "localhost",
        "127.0.0.1",
        "[::1]",
        "localhost:7410",
        "127.0.0.1:7410",
        "[::1]:7410",
    ] {
        add(loopback.to_owned());
    }
    result
}

/// Resolves allowed CORS origins, incorporating explicit origins, environment variables, and API origin.
///
/// The list is built once here and not read again while the server runs.
fn resolve_allowed_origins(
    env: &ServeEnv,
    flags: &[String],
    api_origin: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut result: Vec<String> = Vec::new();
    for given in env.entries("--allowed-origin", flags, serve_env::ALLOWED_ORIGINS) {
        let origin = origin_setting(given)?;
        if !result.contains(&origin) {
            result.push(origin);
        }
    }
    if let Some(origin) = api_origin
        && !result.iter().any(|known| known == origin)
    {
        result.push(origin.to_owned());
    }
    Ok(result)
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
  artifree-server serve --one-shot-ttl-seconds <n> [--session-root <dir>] [options]
  artifree-server backup --data-dir <dir> --output <file>
  artifree-server restore --input <file> --data-dir <dir>
  artifree-server worker run --data-dir <dir> [--once]
  artifree-server --describe | --version | --help

serve runs the HTTP API and a separate preview content origin until Ctrl+C or
SIGTERM. When both listeners are ready it prints one JSON line on stdout:
{{\"event\":\"ready\",\"api_url\":...,\"content_origin\":...,\"token_file\":...}}
Clients authenticate with the bearer token stored in token_file. The line also
has mode (persistent or one_shot) and data_dir.

Options:
  --data-dir <dir>              SQLite database and access token. It is kept.
                                On Unix it is created with mode 700, and a
                                directory other users can access is refused.
                                Give this or --one-shot-ttl-seconds, not both.
  --one-shot-ttl-seconds <n>    A one-shot session: a new private temporary data
                                directory, empty, with a new token, deleted when
                                the process ends (after n seconds, 1-604800, in
                                total from the start of serve; on SIGINT or
                                SIGTERM; on a start-up failure). Nothing is kept,
                                and a session is never resumed. There is no
                                default lifetime. It adds session_ttl_seconds and
                                sweep to the readiness line. A tunnel lifetime is
                                separate and not implemented.
  --session-root <dir>          Where the session directory is created, with
                                --one-shot-ttl-seconds [default: the system
                                temporary directory]. It must exist. The next
                                one-shot start under it removes the directories
                                of sessions that were killed, when their owner is
                                proven gone (Linux only; see decision 0039).
  --web-dir <dir>               Directory holding production web assets (or ARTIFREE_WEB_DIR)
  --lan <ip>                    Bind listeners to explicit LAN IP (or ARTIFREE_LAN)
  --api-listen <addr:port>      API address [default: {api}]
  --loopback-listen <addr:port> Dedicated loopback API address for MCP in LAN mode
  --content-listen <addr:port>  Preview address [default: {content}]
  --content-origin <origin>     Public preview origin, if proxied (or ARTIFREE_CONTENT_ORIGIN)
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
                                (or ARTIFREE_FRAME_ANCESTORS)
  --preview-ttl-seconds <n>     Preview link lifetime, 1-86400 [default: 900]
  --request-burst <n>           Requests accepted at once [default: {burst}]
  --requests-per-minute <n>     Sustained request rate [default: {rate}]
  --concurrent-requests <n>     Requests running at once [default: {concurrent}]
  --change-streams <n>          Open change streams [default: {streams}]

A flag wins over its environment variable, and an empty variable counts as
unset. An invalid value stops the server; the message names the flag or
variable and the rule, never the value. Origins are http(s)://host[:port] with
no credentials, path, query or fragment. The allowed hosts and origins, trusted
proxies, and frame ancestors are read once at start; changing them needs a
restart.

Request limits apply per actor to HTTP and MCP together; a refused request
gets 429 rate_limited with Retry-After and is not processed. Opening a change
stream counts as one request; an open stream holds one of the actor's
change stream slots instead of a running request.

backup writes a consistent copy of the database to a new file, also while
the server runs, and proves that it restores. restore builds a new data
directory from such a file. Each prints one JSON line on success; see
`backup --help`, `restore --help`, and docs/backup-restore.md.

worker run runs the persistent jobs of a data directory in a separate process
(decision 0030) until the queue is empty with --once, or until Ctrl+C or
SIGTERM. It prints JSON lines; see `worker --help`.

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
    use serve_env::{
        ALLOW_REMOTE_MCP, ALLOWED_HOSTS, ALLOWED_ORIGINS, API_ORIGIN, CONTENT_ORIGIN,
        FRAME_ANCESTORS, LAN, LOOPBACK_LISTEN, TRUSTED_PROXIES, WEB_DIR,
    };

    const SECRET: &str = "hunter2";

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// Parses with no environment at all, whatever the test process holds.
    fn parse(list: &[&str]) -> Result<ServeOptions, String> {
        parse_with(&ServeEnv::default(), list)
    }

    fn parse_with(env: &ServeEnv, list: &[&str]) -> Result<ServeOptions, String> {
        let mut all = vec!["--data-dir", "/tmp"];
        all.extend_from_slice(list);
        parse_serve(&args(&all), env)
    }

    fn refusal(env: &ServeEnv, list: &[&str]) -> String {
        match parse_with(env, list) {
            Ok(options) => panic!("expected a refusal, got {options:?}"),
            Err(message) => message,
        }
    }

    #[test]
    fn unspecified_lan_is_rejected() {
        for address in ["0.0.0.0", "::", "[::]"] {
            let error = refusal(&ServeEnv::default(), &["--lan", address]);
            assert!(
                error.starts_with("--lan must be a specific IP address")
                    && error.contains("not an unspecified address"),
                "{address}: {error}"
            );
        }
        let from_variable = refusal(&ServeEnv::default().with(LAN, "0.0.0.0"), &[]);
        assert!(
            from_variable.starts_with("ARTIFREE_LAN must be"),
            "{from_variable}"
        );
    }

    #[test]
    fn bracketed_and_unbracketed_lan_are_accepted() -> Result<(), String> {
        let loopback = parse(&["--lan", "[::1]"])?;
        assert_eq!(loopback.api_address.to_string(), "[::1]:7410");
        assert_eq!(
            loopback.content_origin.as_deref(),
            Some("http://[::1]:7411")
        );

        let lan = parse(&["--lan", "[2001:db8::1]"])?;
        assert_eq!(lan.api_address.to_string(), "[2001:db8::1]:7410");
        assert_eq!(
            lan.content_origin.as_deref(),
            Some("http://[2001:db8::1]:7411")
        );

        let v4 = parse(&["--lan", "192.168.1.50"])?;
        assert_eq!(v4.api_address.to_string(), "192.168.1.50:7410");
        assert_eq!(
            v4.content_origin.as_deref(),
            Some("http://192.168.1.50:7411")
        );
        Ok(())
    }

    #[test]
    fn content_origin_validation_and_normalization() -> Result<(), String> {
        let options = parse(&["--content-origin", "http://[0:0:0:0:0:0:0:1]:80/"])?;
        assert_eq!(options.content_origin.as_deref(), Some("http://[::1]"));

        let error = refusal(
            &ServeEnv::default(),
            &["--content-origin", "not-a-valid-url"],
        );
        assert!(
            error.starts_with("--content-origin must be a valid HTTP or HTTPS origin"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn api_origin_and_content_origin_separation() -> Result<(), String> {
        let options = parse(&[
            "--api-origin",
            "https://artifree.example.com",
            "--content-origin",
            "https://preview.example.com",
            "--trusted-proxy",
            "127.0.0.1,10.0.0.0/8",
            "--allow-remote-mcp",
        ])?;
        assert_eq!(
            options.api_origin.as_deref(),
            Some("https://artifree.example.com")
        );
        assert_eq!(
            options.content_origin.as_deref(),
            Some("https://preview.example.com")
        );
        assert!(options.allow_remote_mcp);
        assert_eq!(options.trusted_proxies.len(), 2);
        assert!(
            options
                .allowed_hosts
                .iter()
                .any(|h| h == "artifree.example.com")
        );

        let conflict = refusal(
            &ServeEnv::default(),
            &[
                "--api-origin",
                "https://artifree.example.com",
                "--content-origin",
                "https://ARTIFREE.example.com:443",
            ],
        );
        assert!(
            conflict.contains("--content-origin must differ from --api-origin"),
            "{conflict}"
        );

        // A browser reads 0x7f.0.0.1 as 127.0.0.1, one origin with the API's.
        let encoded = refusal(
            &ServeEnv::default(),
            &[
                "--api-origin",
                "http://127.0.0.1:7410",
                "--content-origin",
                "http://0x7f.0.0.1:7410",
            ],
        );
        assert!(encoded.contains("--content-origin"), "{encoded}");
        Ok(())
    }

    #[test]
    fn the_content_origin_variable_is_used_without_a_flag() -> Result<(), String> {
        let env = ServeEnv::default().with(CONTENT_ORIGIN, "https://Preview.example:443");
        let options = parse_with(&env, &[])?;
        assert_eq!(
            options.content_origin.as_deref(),
            Some("https://preview.example")
        );
        // An explicit --content-listen does not by itself discard the variable.
        let listen = parse_with(&env, &["--content-listen", "0.0.0.0:7411"])?;
        assert_eq!(
            listen.content_origin.as_deref(),
            Some("https://preview.example")
        );
        Ok(())
    }

    #[test]
    fn flags_win_over_the_new_variables() -> Result<(), String> {
        let env = ServeEnv::default()
            .with(CONTENT_ORIGIN, "https://from-env.example")
            .with(FRAME_ANCESTORS, "https://env-ui.example");
        let options = parse_with(
            &env,
            &[
                "--content-origin",
                "https://from-flag.example",
                "--frame-ancestors",
                "https://flag-ui.example",
            ],
        )?;
        assert_eq!(
            options.content_origin.as_deref(),
            Some("https://from-flag.example")
        );
        assert_eq!(options.frame_ancestors, "https://flag-ui.example");
        Ok(())
    }

    #[test]
    fn an_empty_variable_is_unset_but_an_empty_flag_is_refused() -> Result<(), String> {
        let env = ServeEnv::default()
            .with(CONTENT_ORIGIN, "")
            .with(FRAME_ANCESTORS, "");
        let options = parse_with(&env, &[])?;
        assert_eq!(options.content_origin, None);
        assert_eq!(options.frame_ancestors, serve::DEFAULT_FRAME_ANCESTORS);

        let empty_flag = refusal(&ServeEnv::default(), &["--frame-ancestors", ""]);
        assert!(
            empty_flag.starts_with("--frame-ancestors must be"),
            "{empty_flag}"
        );
        let empty_flag = refusal(&ServeEnv::default(), &["--content-origin", " "]);
        assert!(
            empty_flag.starts_with("--content-origin must be"),
            "{empty_flag}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_variable_that_is_not_utf8_matters_only_without_its_flag() -> Result<(), String> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let bytes = || OsString::from_vec(vec![b'h', 0xff]);
        let env = ServeEnv::default()
            .with(CONTENT_ORIGIN, bytes())
            .with(FRAME_ANCESTORS, bytes());
        let flagged = parse_with(
            &env,
            &[
                "--content-origin",
                "https://preview.example",
                "--frame-ancestors",
                "'self'",
            ],
        )?;
        assert_eq!(
            flagged.content_origin.as_deref(),
            Some("https://preview.example")
        );
        let content = refusal(&env, &["--frame-ancestors", "'self'"]);
        assert!(
            content.starts_with("ARTIFREE_CONTENT_ORIGIN is not valid UTF-8"),
            "{content}"
        );
        let ancestors = refusal(&env, &["--content-origin", "https://preview.example"]);
        assert!(
            ancestors.starts_with("ARTIFREE_FRAME_ANCESTORS is not valid UTF-8"),
            "{ancestors}"
        );
        Ok(())
    }

    #[test]
    fn frame_ancestors_accepts_the_documented_sources() -> Result<(), String> {
        for list in [
            "*",
            "'self'",
            "https:",
            "https://ui.example http://localhost:*",
            "  https://ui.example   'self' ",
        ] {
            let from_flag = parse(&["--frame-ancestors", list])?.frame_ancestors;
            let from_env =
                parse_with(&ServeEnv::default().with(FRAME_ANCESTORS, list), &[])?.frame_ancestors;
            assert_eq!(from_flag, from_env, "list {list:?}");
            assert_eq!(
                from_flag,
                list.split_whitespace().collect::<Vec<_>>().join(" ")
            );
        }
        Ok(())
    }

    #[test]
    fn frame_ancestors_refuses_directives_and_commas_without_echoing_them() {
        for list in [
            "",
            "   ",
            "https://ui.example; script-src hunter2",
            "https://ui.example,https://other.example",
            "https://user:hunter2@ui.example,",
            "https://ui.example\u{7f}hunter2",
            "https://ünï.example/hunter2",
        ] {
            let flag = refusal(&ServeEnv::default(), &["--frame-ancestors", list]);
            assert!(
                flag.starts_with("--frame-ancestors must be"),
                "{list:?}: {flag}"
            );
            let mut messages = vec![flag];
            if !list.is_empty() {
                messages.push(refusal(
                    &ServeEnv::default().with(FRAME_ANCESTORS, list),
                    &[],
                ));
            }
            for message in messages {
                assert!(!message.contains(SECRET), "{list:?}: {message}");
            }
        }
    }

    /// Credentialed text that fails an early check must not appear anywhere.
    const HOSTILE_ORIGINS: [&str; 9] = [
        "ftp://user:hunter2@leak",
        "user:hunter2@leak",
        "hunter2",
        "https://user:hunter2@leak:65536",
        "https://user:hunter2@leak:notaport",
        "https://user:hunter2@",
        "https://user:hunter2@[not-an-ip]",
        "https://user:hunter2@ho st",
        "https//user:p%40ss-hunter2@leak",
    ];

    #[test]
    fn origin_errors_never_repeat_a_credentialed_value() {
        for value in HOSTILE_ORIGINS {
            for flag in ["--content-origin", "--api-origin", "--allowed-origin"] {
                let message = refusal(&ServeEnv::default(), &[flag, value]);
                assert!(message.starts_with(flag), "{flag} {value}: {message}");
                assert!(!message.contains(SECRET), "{flag} {value}: {message}");
                assert!(!message.contains("leak"), "{flag} {value}: {message}");
            }
            for variable in [CONTENT_ORIGIN, API_ORIGIN, ALLOWED_ORIGINS] {
                let message = refusal(&ServeEnv::default().with(variable, value), &[]);
                assert!(
                    message.starts_with(variable),
                    "{variable} {value}: {message}"
                );
                assert!(!message.contains(SECRET), "{variable} {value}: {message}");
            }
        }
        // The same inside a comma list, after an entry that is fine.
        let message = refusal(
            &ServeEnv::default().with(ALLOWED_ORIGINS, "https://ok.example, ftp://u:hunter2@h"),
            &[],
        );
        assert!(
            message.starts_with(ALLOWED_ORIGINS) && !message.contains(SECRET),
            "{message}"
        );
    }

    #[test]
    fn other_settings_never_repeat_a_credentialed_value() {
        let hostile = "https://user:hunter2@leak";
        for flag in [
            "--lan",
            "--api-listen",
            "--loopback-listen",
            "--content-listen",
            "--trusted-proxy",
            "--preview-ttl-seconds",
            "--request-burst",
            "--requests-per-minute",
            "--concurrent-requests",
            "--change-streams",
        ] {
            let message = refusal(&ServeEnv::default(), &[flag, hostile]);
            assert!(!message.contains(SECRET), "{flag}: {message}");
            assert!(!message.contains("leak"), "{flag}: {message}");
        }
        for variable in [LAN, LOOPBACK_LISTEN, TRUSTED_PROXIES] {
            let message = refusal(&ServeEnv::default().with(variable, hostile), &[]);
            assert!(message.starts_with(variable), "{variable}: {message}");
            assert!(!message.contains(SECRET), "{variable}: {message}");
        }
        let cidr = refusal(
            &ServeEnv::default(),
            &["--trusted-proxy", "10.0.0.0/hunter2,127.0.0.1"],
        );
        assert!(
            cidr.starts_with("--trusted-proxy entries") && !cidr.contains(SECRET),
            "{cidr}"
        );
    }

    #[test]
    fn an_unrecognized_option_is_named_only_when_it_looks_like_one() {
        let named = |argument: &str| {
            let Err(message) = parse_serve(&args(&[argument]), &ServeEnv::default()) else {
                panic!("{argument} must be refused");
            };
            assert!(!message.contains(SECRET), "{argument}: {message}");
            assert!(!message.contains("leak"), "{argument}: {message}");
            message
        };
        for argument in [
            "--content-origin=https://user:hunter2@leak",
            "--no-such-flag=hunter2",
        ] {
            let message = named(argument);
            let name = argument.split('=').next().unwrap_or_default();
            assert!(
                message.contains(&format!("serve option {name};")),
                "{argument}: {message}"
            );
        }
        for argument in [
            "--content-origin https://user:hunter2@leak",
            "--content-origin:https://user:hunter2@leak",
            "--CONTENT-ORIGIN=https://user:hunter2@leak",
            "-content-origin=https://user:hunter2@leak",
            "https://user:hunter2@leak",
            "hunter2",
            "--=hunter2",
            "--",
            "--hunter2$=x",
        ] {
            let message = named(argument);
            assert!(
                message.contains("An argument is not a serve option"),
                "{argument}: {message}"
            );
        }
    }

    #[test]
    fn a_missing_value_names_the_flag_only() {
        let Err(message) = parse_serve(&args(&["--content-origin"]), &ServeEnv::default()) else {
            panic!("a flag without a value must be refused");
        };
        assert!(
            message.contains("--content-origin requires a value"),
            "{message}"
        );
    }

    #[test]
    fn an_invalid_port_never_reaches_the_cors_allow_list() -> Result<(), String> {
        for entry in [
            "https://ui.example:65536",
            "https://ui.example:abc",
            "https://ui.example:",
            "https://ui.example:0",
            "https://ui.example:08080x",
            "https://ui.example:-1",
        ] {
            let flag = refusal(&ServeEnv::default(), &["--allowed-origin", entry]);
            assert!(
                flag.starts_with("--allowed-origin must be"),
                "{entry}: {flag}"
            );
            let mixed = refusal(
                &ServeEnv::default(),
                &["--allowed-origin", &format!("https://ok.example,{entry}")],
            );
            assert!(
                mixed.starts_with("--allowed-origin must be"),
                "{entry}: {mixed}"
            );
            let variable = refusal(&ServeEnv::default().with(ALLOWED_ORIGINS, entry), &[]);
            assert!(variable.starts_with(ALLOWED_ORIGINS), "{entry}: {variable}");
        }
        for entry in ["*", "ui.example", "https://ui.example/app", "null"] {
            refusal(&ServeEnv::default(), &["--allowed-origin", entry]);
        }
        let options = parse_with(
            &ServeEnv::default().with(
                ALLOWED_ORIGINS,
                "HTTPS://UI.example:443, https://b.example:8443/",
            ),
            &["--allowed-origin", "https://ui.example"],
        )?;
        assert_eq!(
            options.allowed_origins,
            ["https://ui.example", "https://b.example:8443"]
        );
        Ok(())
    }

    #[test]
    fn older_variables_keep_their_meaning() -> Result<(), String> {
        let env = ServeEnv::default()
            .with(WEB_DIR, "/srv/web")
            .with(ALLOW_REMOTE_MCP, "1")
            .with(ALLOWED_HOSTS, "Artifree.example, ,artifree.EXAMPLE")
            .with(TRUSTED_PROXIES, "10.0.0.0/8")
            .with(LOOPBACK_LISTEN, "127.0.0.1:9000")
            .with(API_ORIGIN, "https://api.example");
        let options = parse_with(&env, &[])?;
        assert_eq!(
            options.web_dir.as_deref(),
            Some(std::path::Path::new("/srv/web"))
        );
        assert!(options.allow_remote_mcp);
        assert_eq!(options.trusted_proxies.len(), 1);
        assert_eq!(
            options.loopback_api_address,
            Some("127.0.0.1:9000".parse().map_err(|_| "addr")?)
        );
        assert_eq!(options.api_origin.as_deref(), Some("https://api.example"));
        let hosts: Vec<&str> = options.allowed_hosts.iter().map(String::as_str).collect();
        assert_eq!(&hosts[..2], ["Artifree.example", "api.example"]);
        assert!(
            options
                .allowed_origins
                .contains(&"https://api.example".to_owned())
        );
        // An empty or false-looking value does not enable remote MCP.
        let off = parse_with(&ServeEnv::default().with(ALLOW_REMOTE_MCP, "yes"), &[])?;
        assert!(!off.allow_remote_mcp);
        Ok(())
    }

    fn parse_only(list: &[&str]) -> Result<ServeOptions, String> {
        parse_serve(&args(list), &ServeEnv::default())
    }

    #[test]
    fn a_one_shot_session_needs_an_explicit_lifetime_and_has_no_default() -> Result<(), String> {
        let options = parse_only(&["--one-shot-ttl-seconds", "90"])?;
        assert!(matches!(
            options.data,
            DataLocation::OneShot(OneShot { ttl, session_root: None }) if ttl == Duration::from_secs(90)
        ));
        let rooted = parse_only(&["--session-root", "/var/tmp", "--one-shot-ttl-seconds", "1"])?;
        assert!(matches!(
            rooted.data,
            DataLocation::OneShot(OneShot { ttl, session_root: Some(ref root) })
                if ttl == Duration::from_secs(1) && root.as_path() == std::path::Path::new("/var/tmp")
        ));
        let Err(neither) = parse_only(&[]) else {
            return Err("serve with neither storage option must be refused".into());
        };
        assert!(
            neither.contains("--data-dir") && neither.contains("--one-shot-ttl-seconds"),
            "{neither}"
        );
        Ok(())
    }

    #[test]
    fn the_one_shot_lifetime_accepts_one_second_to_seven_days_only() -> Result<(), String> {
        for ok in ["1", "604800"] {
            parse_only(&["--one-shot-ttl-seconds", ok])?;
        }
        for bad in [
            "0",
            "604801",
            "-1",
            "1.5",
            "",
            "1h",
            "+5",
            "99999999999999999999",
        ] {
            let Err(message) = parse_only(&["--one-shot-ttl-seconds", bad]) else {
                return Err(format!("{bad:?} must be refused"));
            };
            assert!(
                message
                    .starts_with("--one-shot-ttl-seconds must be a whole number from 1 to 604800"),
                "{bad:?}: {message}"
            );
        }
        Ok(())
    }

    #[test]
    fn contradictory_storage_options_are_refused_with_the_reason() {
        let cases: [(&[&str], &str); 4] = [
            (
                &["--data-dir", "/tmp/d", "--one-shot-ttl-seconds", "5"],
                "cannot be combined",
            ),
            (
                &[
                    "--one-shot-ttl-seconds",
                    "5",
                    "--data-dir",
                    "/tmp/d",
                    "--session-root",
                    "/tmp",
                ],
                "cannot be combined",
            ),
            (
                &["--data-dir", "/tmp/d", "--session-root", "/tmp"],
                "--session-root only applies with --one-shot-ttl-seconds",
            ),
            (
                &["--session-root", "/tmp"],
                "--session-root only applies with --one-shot-ttl-seconds",
            ),
        ];
        for (list, expected) in cases {
            let Err(message) = parse_only(list) else {
                panic!("{list:?} must be refused");
            };
            assert!(message.contains(expected), "{list:?}: {message}");
        }
    }

    #[test]
    fn a_persistent_data_dir_parses_as_before() -> Result<(), String> {
        let options = parse_only(&["--data-dir", "/srv/artifree"])?;
        assert!(matches!(
            options.data,
            DataLocation::Persistent { ref data_dir } if data_dir.as_path() == std::path::Path::new("/srv/artifree")
        ));
        Ok(())
    }
}
