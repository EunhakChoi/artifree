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
use artifree_server::serve::{self, ServeOptions};

mod maintenance;

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
    // Paths may be any bytes the platform allows, so these two commands read
    // their arguments as they are.
    if let Some((command, rest)) = args.split_first() {
        if command == "backup" {
            return maintenance::backup_command(rest);
        }
        if command == "restore" {
            return maintenance::restore_command(rest);
        }
    }
    let args = args
        .into_iter()
        .map(|argument| {
            argument.into_string().map_err(|_| {
                // Not quoted: a value may be a secret (see `unrecognized_option`).
                "Unknown arguments. An argument is not valid UTF-8.".to_owned()
            })
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

/// Whether `text` is a plain long option name: `--` and then only lowercase
/// letters, digits, and `-`.
fn is_option_name(text: &str) -> bool {
    text.strip_prefix("--").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// Describes an argument that is not a serve option without quoting a value.
///
/// A mistyped `--content-origin=https://user:secret@host` must not put the
/// secret into the error. The text is named only when it is a plain option
/// name, or such a name followed by `=` (the value after `=` is never shown).
/// Anything else, such as a flag and its value joined by a space or a colon
/// inside one argument, or a bare value, is not quoted at all.
fn unrecognized_option(argument: &str) -> String {
    let name = argument.split('=').next().unwrap_or_default();
    let what = if is_option_name(name) {
        format!("Unrecognized serve option {name}")
    } else {
        "Unexpected argument that is not a plain option name (not shown, in case it holds a secret)"
            .to_owned()
    };
    format!("Unknown arguments. {what}; use --help.")
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
            other => return Err(unrecognized_option(other)),
        }
    }
    Ok(flags)
}

/// Every environment variable the serve options read, taken once.
///
/// Parsing takes this as a value instead of reading the process environment,
/// so tests give it exactly the variables they mean and a developer's own
/// `ARTIFREE_*` settings cannot change their result.
///
/// Decision 0028 added `content_origin` and `frame_ancestors`: an empty value
/// is unset, and a value that is not valid UTF-8 is refused, but only when no
/// flag overrides it (a flag in effect hides its variable). The other fields
/// keep their earlier handling: they hold the raw value, and each resolver
/// decides what empty means; a value that is not valid UTF-8 is ignored.
#[derive(Debug, Default)]
struct ServeEnv {
    content_origin: Option<Result<String, String>>,
    frame_ancestors: Option<Result<String, String>>,
    web_dir: Option<String>,
    allow_remote_mcp: Option<String>,
    lan: Option<String>,
    loopback_listen: Option<String>,
    api_origin: Option<String>,
    trusted_proxies: Option<String>,
    allowed_hosts: Option<String>,
    allowed_origins: Option<String>,
}

impl ServeEnv {
    fn from_process() -> Self {
        let lenient = |name: &str| std::env::var(name).ok();
        Self {
            content_origin: env_setting("ARTIFREE_CONTENT_ORIGIN"),
            frame_ancestors: env_setting("ARTIFREE_FRAME_ANCESTORS"),
            web_dir: lenient("ARTIFREE_WEB_DIR"),
            allow_remote_mcp: lenient("ARTIFREE_ALLOW_REMOTE_MCP"),
            lan: lenient("ARTIFREE_LAN"),
            loopback_listen: lenient("ARTIFREE_LOOPBACK_LISTEN"),
            api_origin: lenient("ARTIFREE_API_ORIGIN"),
            trusted_proxies: lenient("ARTIFREE_TRUSTED_PROXIES"),
            allowed_hosts: lenient("ARTIFREE_ALLOWED_HOSTS"),
            allowed_origins: lenient("ARTIFREE_ALLOWED_ORIGINS"),
        }
    }
}

/// Reads one environment variable. Unset and empty are `None`; a value that
/// is not valid UTF-8 is kept as the error to report if the variable is used.
fn env_setting(name: &str) -> Option<Result<String, String>> {
    match std::env::var(name) {
        Ok(value) if value.is_empty() => None,
        Ok(value) => Some(Ok(value)),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => Some(Err(format!("{name} is not valid UTF-8."))),
    }
}

/// A setting value and the flag or variable it came from, so errors name
/// what the operator actually wrote.
struct Sourced {
    value: String,
    source: &'static str,
}

/// A normalized origin and the flag or variable it came from.
struct OriginSetting {
    origin: String,
    source: &'static str,
}

/// Picks the flag over the environment variable; the default is the caller's
/// fallback when both are absent. The variable is only examined when no flag
/// is given, so an unusable variable does not matter when a flag overrides it.
fn flag_over_env(
    flag: Option<String>,
    flag_name: &'static str,
    env: Option<Result<String, String>>,
    env_name: &'static str,
) -> Result<Option<Sourced>, String> {
    if let Some(value) = flag {
        return Ok(Some(Sourced {
            value,
            source: flag_name,
        }));
    }
    env.map(|setting| {
        setting.map(|value| Sourced {
            value,
            source: env_name,
        })
    })
    .transpose()
}

fn parse_serve(arguments: &[String]) -> Result<ServeOptions, String> {
    parse_serve_with(arguments, ServeEnv::from_process())
}

fn parse_serve_with(arguments: &[String], env: ServeEnv) -> Result<ServeOptions, String> {
    let flags = parse_serve_flags(arguments)?;

    let explicit_web_dir = match flags.cli_web_dir {
        Some(dir) => Some(dir),
        None => env.web_dir.filter(|s| !s.is_empty()).map(PathBuf::from),
    };

    let lan_ip = resolve_lan_ip(flags.cli_lan, env.lan)?;
    let (default_api, default_content, default_origin) = resolve_lan_defaults(lan_ip);

    let api_str = flags.explicit_api_listen.unwrap_or(default_api);
    let content_str = flags
        .explicit_content_listen
        .clone()
        .unwrap_or(default_content);
    let resolved_content_origin = resolve_content_origin(
        flag_over_env(
            flags.explicit_content_origin,
            "--content-origin",
            env.content_origin,
            "ARTIFREE_CONTENT_ORIGIN",
        )?,
        flags.explicit_content_listen.is_some(),
        default_origin,
    )?;
    let resolved_api_origin = resolve_api_origin(flags.explicit_api_origin, env.api_origin)?;

    if let (Some(api), Some(content)) = (&resolved_api_origin, &resolved_content_origin)
        && api.origin == content.origin
    {
        return Err(format!(
            "{} must differ from {} to maintain preview sandbox isolation",
            content.source, api.source
        ));
    }
    let resolved_api_origin = resolved_api_origin.map(|setting| setting.origin);
    let resolved_content_origin = resolved_content_origin.map(|setting| setting.origin);

    let trusted_proxies =
        resolve_trusted_proxies(&flags.cli_trusted_proxies, env.trusted_proxies.as_deref())?;
    let allowed_hosts = resolve_allowed_hosts(
        flags.cli_allowed_hosts,
        env.allowed_hosts.as_deref(),
        resolved_api_origin.as_deref(),
        lan_ip,
    );
    let allowed_origins = resolve_allowed_origins(
        &flags.cli_allowed_origins,
        env.allowed_origins.as_deref(),
        resolved_api_origin.as_deref(),
    )?;
    let allow_remote_mcp = flags.cli_allow_remote_mcp
        || env
            .allow_remote_mcp
            .is_some_and(|v| v == "true" || v == "1");

    let frame_ancestors = derive_frame_ancestors(
        flag_over_env(
            flags.explicit_frame_ancestors,
            "--frame-ancestors",
            env.frame_ancestors,
            "ARTIFREE_FRAME_ANCESTORS",
        )?,
        &api_str,
        resolved_api_origin.as_deref(),
    )?;

    let address = |flag: &str, value: &str| {
        value
            .parse::<SocketAddr>()
            .map_err(|_| format!("{flag} must be an IP address and port, such as 127.0.0.1:7410."))
    };

    let loopback_api_address =
        resolve_loopback_listen(flags.explicit_loopback_listen, env.loopback_listen)?;

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
fn resolve_lan_ip(
    cli_lan: Option<String>,
    env_lan: Option<String>,
) -> Result<Option<IpAddr>, String> {
    let explicit_lan = match cli_lan {
        Some(lan) => Some((lan, "--lan")),
        None => env_lan
            .filter(|s| !s.is_empty())
            .map(|lan| (lan, "ARTIFREE_LAN")),
    };
    match explicit_lan {
        Some((val, source)) => {
            let unbracketed = val
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(&val);
            let ip: IpAddr = unbracketed
                .parse()
                .map_err(|_| format!("{source} must be an IP address (IPv4 or IPv6)."))?;
            if ip.is_unspecified() {
                return Err(format!(
                    "{source} must be a specific IP address (IPv4 or IPv6), not an unspecified address ({val}). Use --api-listen and --content-listen to bind to wildcard addresses."
                ));
            }
            Ok(Some(ip))
        }
        None => Ok(None),
    }
}

/// Resolves preview content origin, validating and normalizing explicit origin.
fn resolve_content_origin(
    explicit: Option<Sourced>,
    explicit_content_listen: bool,
    default_origin: Option<String>,
) -> Result<Option<OriginSetting>, String> {
    match explicit {
        Some(Sourced { value, source }) => {
            let canon = artifree_server::http::normalize_origin(&value)
                .map_err(|e| format!("{source} must be a valid HTTP or HTTPS origin: {e}"))?;
            Ok(Some(OriginSetting {
                origin: canon,
                source,
            }))
        }
        None => {
            if explicit_content_listen {
                Ok(None)
            } else {
                Ok(default_origin.map(|origin| OriginSetting {
                    origin,
                    source: "the preview origin derived from --lan",
                }))
            }
        }
    }
}

/// Resolves an optional loopback listener for MCP peer isolation in LAN mode.
fn resolve_loopback_listen(
    explicit: Option<String>,
    env_listen: Option<String>,
) -> Result<Option<SocketAddr>, String> {
    let raw = match explicit {
        Some(addr) => Some((addr, "--loopback-listen")),
        None => env_listen
            .filter(|s| !s.is_empty())
            .map(|addr| (addr, "ARTIFREE_LOOPBACK_LISTEN")),
    };
    match raw {
        Some((val, source)) => {
            let addr = val.parse::<SocketAddr>().map_err(|_| {
                format!("{source} must be an IP address and port, such as 127.0.0.1:7410.")
            })?;
            if !addr.ip().is_loopback() {
                return Err(format!(
                    "{source} must be a loopback IP address, such as 127.0.0.1 or ::1."
                ));
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
///
/// # Errors
///
/// When an explicit value is not a plain source list; see
/// [`validate_frame_ancestors`].
fn derive_frame_ancestors(
    explicit: Option<Sourced>,
    api_address: &str,
    api_origin: Option<&str>,
) -> Result<String, String> {
    if let Some(Sourced { value, source }) = explicit {
        return validate_frame_ancestors(&value, source);
    }
    if let Some(origin) = api_origin {
        return Ok(format!("{origin} {}", serve::DEFAULT_FRAME_ANCESTORS));
    }
    let Ok(addr) = api_address.parse::<SocketAddr>() else {
        return Ok(serve::DEFAULT_FRAME_ANCESTORS.to_owned());
    };
    let ip = addr.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        Ok(serve::DEFAULT_FRAME_ANCESTORS.to_owned())
    } else {
        let formatted_ip = if ip.is_ipv6() {
            format!("[{ip}]")
        } else {
            format!("{ip}")
        };
        Ok(format!(
            "http://{formatted_ip}:* {}",
            serve::DEFAULT_FRAME_ANCESTORS
        ))
    }
}

/// Accepts a space-separated CSP source list and refuses what would change
/// the policy around it.
///
/// An empty list is refused because browsers ignore an empty
/// `frame-ancestors` directive and then allow every embedder. `;` would start
/// another directive in the preview policy, and `,` is not a separator in
/// this list. The value is otherwise passed through, so source expressions
/// such as `'self'`, `https:`, and `http://host:*` stay available.
fn validate_frame_ancestors(value: &str, source: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "{source} must list at least one source; an empty list would let any page embed previews."
        ));
    }
    if trimmed
        .chars()
        .any(|c| matches!(c, ';' | ',') || !(c.is_ascii_graphic() || c == ' '))
    {
        return Err(format!(
            "{source} must be space-separated sources in visible ASCII without ';' or ','."
        ));
    }
    Ok(trimmed.to_owned())
}

/// Resolves public API origin, validating and normalizing explicit origin.
fn resolve_api_origin(
    explicit: Option<String>,
    env_origin: Option<String>,
) -> Result<Option<OriginSetting>, String> {
    let raw = match explicit {
        Some(origin) => Some((origin, "--api-origin")),
        None => env_origin
            .filter(|s| !s.is_empty())
            .map(|origin| (origin, "ARTIFREE_API_ORIGIN")),
    };
    match raw {
        Some((origin, source)) => {
            let canon = artifree_server::http::normalize_origin(&origin)
                .map_err(|e| format!("{source} must be a valid HTTP or HTTPS origin: {e}"))?;
            Ok(Some(OriginSetting {
                origin: canon,
                source,
            }))
        }
        None => Ok(None),
    }
}

/// Resolves trusted proxy IP/CIDR list from CLI arguments and environment variables.
fn resolve_trusted_proxies(
    cli_proxies: &[String],
    env_proxies: Option<&str>,
) -> Result<Vec<artifree_server::proxy::IpNetwork>, String> {
    let mut result = Vec::new();
    let entries = cli_proxies
        .iter()
        .map(|entry| ("--trusted-proxy", entry.as_str()))
        .chain(env_proxies.map(|entry| ("ARTIFREE_TRUSTED_PROXIES", entry)));
    for (source, entry) in entries {
        for part in entry.split(',') {
            let part = part.trim();
            if !part.is_empty() {
                let net: artifree_server::proxy::IpNetwork = part
                    .parse()
                    .map_err(|e| format!("{source} lists an entry that is not usable: {e}"))?;
                result.push(net);
            }
        }
    }
    Ok(result)
}

/// Resolves allowed hosts, incorporating explicit hosts, environment variables, API origin host, and LAN IP.
fn resolve_allowed_hosts(
    cli_hosts: Vec<String>,
    env_hosts: Option<&str>,
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
    if let Some(env_hosts) = env_hosts {
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
///
/// Each entry is normalized like the origin of a request, so `https://UI.example.app/`
/// and `https://ui.example.app:443` match the browser's `https://ui.example.app`.
/// An entry that is not an HTTP or HTTPS origin is refused instead of
/// silently never matching.
fn resolve_allowed_origins(
    cli_origins: &[String],
    env_origins: Option<&str>,
    api_origin: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut result: Vec<String> = Vec::new();
    let mut add = |source: &str, entry: &str| -> Result<(), String> {
        for part in entry.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let canon = artifree_server::http::normalize_origin(part)
                .map_err(|e| format!("{source} must list HTTP or HTTPS origins: {e}"))?;
            // A CORS origin has no path, query, or fragment; normalizing drops
            // them and would quietly grant what was not written. Checked on the
            // text itself, not on a second parse that could disagree.
            let after_scheme = part.split_once("://").map_or("", |(_, rest)| rest);
            let after_scheme = after_scheme.strip_suffix('/').unwrap_or(after_scheme);
            if after_scheme.contains(['/', '?', '#']) {
                return Err(format!(
                    "{source} must list origins without a path, query, or fragment, such as https://ui.example.app."
                ));
            }
            if !result.contains(&canon) {
                result.push(canon);
            }
        }
        Ok(())
    };
    for entry in cli_origins {
        add("--allowed-origin", entry)?;
    }
    if let Some(env_origins) = env_origins {
        add("ARTIFREE_ALLOWED_ORIGINS", env_origins)?;
    }
    if let Some(origin) = api_origin
        && !result.contains(&origin.to_owned())
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
  artifree-server backup --data-dir <dir> --output <file>
  artifree-server restore --input <file> --data-dir <dir>
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
  --frame-ancestors <sources>   CSP frame-ancestors, replacing the default list
                                (or ARTIFREE_FRAME_ANCESTORS) [default: {ancestors}]
  --preview-ttl-seconds <n>     Preview link lifetime, 1-86400 [default: 900]
  --request-burst <n>           Requests accepted at once [default: {burst}]
  --requests-per-minute <n>     Sustained request rate [default: {rate}]
  --concurrent-requests <n>     Requests running at once [default: {concurrent}]
  --change-streams <n>          Open change streams [default: {streams}]

Where an option names an environment variable, the flag wins over the variable
and the variable over the default; an empty variable counts as unset. The list
options (--trusted-proxy, --allowed-host, --allowed-origin) use both, and
--allow-remote-mcp is on if either the flag or the variable enables it.

Request limits apply per actor to HTTP and MCP together; a refused request
gets 429 rate_limited with Retry-After and is not processed. Opening a change
stream counts as one request; an open stream holds one of the actor's
change stream slots instead of a running request.

backup writes a consistent copy of the database to a new file, also while
the server runs, and proves that it restores. restore builds a new data
directory from such a file. Each prints one JSON line on success; see
`backup --help`, `restore --help`, and docs/backup-restore.md.

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

    /// Parses with no environment variables, whatever this machine has set.
    fn parse_serve(arguments: &[String]) -> Result<ServeOptions, String> {
        parse_serve_with(arguments, ServeEnv::default())
    }

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
    fn serve_env(content_origin: Option<&str>, frame_ancestors: Option<&str>) -> ServeEnv {
        ServeEnv {
            content_origin: content_origin.map(|value| Ok(value.to_owned())),
            frame_ancestors: frame_ancestors.map(|value| Ok(value.to_owned())),
            ..ServeEnv::default()
        }
    }

    fn base_args(extra: &[&str]) -> Vec<String> {
        let mut list = vec!["--data-dir", "/tmp"];
        list.extend_from_slice(extra);
        args(&list)
    }

    #[test]
    fn content_origin_comes_from_the_environment_when_no_flag_is_given() -> Result<(), String> {
        let opts = parse_serve_with(
            &base_args(&[]),
            serve_env(Some("https://Preview.Example.com:443/"), None),
        )?;
        assert_eq!(
            opts.content_origin.as_deref(),
            Some("https://preview.example.com"),
            "the variable is normalized like the flag"
        );
        Ok(())
    }

    #[test]
    fn content_origin_flag_beats_the_environment() -> Result<(), String> {
        let opts = parse_serve_with(
            &base_args(&["--content-origin", "https://flag.example.com"]),
            serve_env(Some("https://env.example.com"), None),
        )?;
        assert_eq!(
            opts.content_origin.as_deref(),
            Some("https://flag.example.com")
        );
        Ok(())
    }

    #[test]
    fn content_origin_environment_beats_the_lan_default() -> Result<(), String> {
        let opts = parse_serve_with(
            &base_args(&["--lan", "192.168.1.50"]),
            serve_env(Some("https://env.example.com"), None),
        )?;
        assert_eq!(
            opts.content_origin.as_deref(),
            Some("https://env.example.com")
        );
        Ok(())
    }

    #[test]
    fn content_origin_defaults_without_flag_or_variable() -> Result<(), String> {
        let opts = parse_serve_with(&base_args(&["--lan", "192.168.1.50"]), ServeEnv::default())?;
        assert_eq!(
            opts.content_origin.as_deref(),
            Some("http://192.168.1.50:7411"),
            "the LAN default is unchanged"
        );
        let plain = parse_serve_with(&base_args(&[]), ServeEnv::default())?;
        assert_eq!(plain.content_origin, None);
        Ok(())
    }

    #[test]
    fn invalid_content_origin_variable_is_refused_and_named() {
        for bad in [
            "not-a-url",
            "ftp://preview.example.com",
            "https://user:pw@preview.example.com",
            "javascript:alert(1)",
            " ",
        ] {
            let err = parse_serve_with(&base_args(&[]), serve_env(Some(bad), None))
                .err()
                .unwrap_or_default();
            assert!(
                err.contains("ARTIFREE_CONTENT_ORIGIN must be a valid HTTP or HTTPS origin"),
                "{bad:?} must be refused naming the variable, got {err:?}"
            );
            assert!(!err.contains("pw"), "an error must not echo credentials");
        }
    }

    #[test]
    fn invalid_content_origin_flag_is_refused_even_when_the_variable_is_valid() {
        let err = parse_serve_with(
            &base_args(&["--content-origin", "ftp://x.example.com"]),
            serve_env(Some("https://env.example.com"), None),
        )
        .err()
        .unwrap_or_default();
        assert!(
            err.contains("--content-origin must be a valid HTTP or HTTPS origin"),
            "the flag is the value in effect, so its error is reported: {err:?}"
        );
    }

    #[test]
    fn content_origin_variable_equal_to_the_api_origin_is_refused() {
        let err = parse_serve_with(
            &base_args(&["--api-origin", "https://artifree.example.com"]),
            serve_env(Some("https://artifree.example.com"), None),
        )
        .err()
        .unwrap_or_default();
        assert!(err.contains("ARTIFREE_CONTENT_ORIGIN must differ from --api-origin"));
    }

    #[test]
    fn frame_ancestors_follow_flag_then_variable_then_default() -> Result<(), String> {
        let from_env = parse_serve_with(
            &base_args(&[]),
            serve_env(None, Some("https://ui.example.app")),
        )?;
        assert_eq!(from_env.frame_ancestors, "https://ui.example.app");

        let from_flag = parse_serve_with(
            &base_args(&["--frame-ancestors", "https://flag.example.app"]),
            serve_env(None, Some("https://ui.example.app")),
        )?;
        assert_eq!(from_flag.frame_ancestors, "https://flag.example.app");

        let default = parse_serve_with(&base_args(&[]), ServeEnv::default())?;
        assert_eq!(default.frame_ancestors, serve::DEFAULT_FRAME_ANCESTORS);

        let with_api_origin = parse_serve_with(
            &base_args(&["--api-origin", "https://api.example.com"]),
            ServeEnv::default(),
        )?;
        assert_eq!(
            with_api_origin.frame_ancestors,
            format!("https://api.example.com {}", serve::DEFAULT_FRAME_ANCESTORS),
            "the API origin is still added to the default list"
        );
        let replaced = parse_serve_with(
            &base_args(&["--api-origin", "https://api.example.com"]),
            serve_env(None, Some("https://ui.example.app")),
        )?;
        assert_eq!(
            replaced.frame_ancestors, "https://ui.example.app",
            "an explicit list replaces the default, as the flag does"
        );
        Ok(())
    }

    #[test]
    fn unsafe_frame_ancestors_are_refused_from_flag_and_variable() {
        let cases = [
            ("", "at least one source"),
            ("   ", "at least one source"),
            ("https://a.example; script-src *", "';'"),
            ("https://a.example,https://b.example", "','"),
            ("https://a.example\nhttps://b.example", "visible ASCII"),
            ("https://ä.example", "visible ASCII"),
        ];
        for (bad, expected) in cases {
            let from_env = parse_serve_with(&base_args(&[]), serve_env(None, Some(bad)))
                .err()
                .unwrap_or_default();
            assert!(
                from_env.contains("ARTIFREE_FRAME_ANCESTORS") && from_env.contains(expected),
                "variable {bad:?} must be refused with {expected:?}, got {from_env:?}"
            );
            let from_flag =
                parse_serve_with(&base_args(&["--frame-ancestors", bad]), ServeEnv::default())
                    .err()
                    .unwrap_or_default();
            assert!(
                from_flag.contains("--frame-ancestors") && from_flag.contains(expected),
                "flag {bad:?} must be refused with {expected:?}, got {from_flag:?}"
            );
        }
    }

    #[test]
    fn frame_ancestors_keep_source_expressions_in_use_today() -> Result<(), String> {
        for good in [
            "'self'",
            "'none'",
            "https:",
            "http://127.0.0.1:* http://localhost:* http://insecure.invalid",
            "https://ui.example.app http://[::1]:*",
        ] {
            let opts = parse_serve_with(
                &base_args(&["--frame-ancestors", good]),
                ServeEnv::default(),
            )?;
            assert_eq!(opts.frame_ancestors, good);
        }
        Ok(())
    }
    #[test]
    fn allowed_origins_are_normalized_like_request_origins() -> Result<(), String> {
        let opts = parse_serve_with(
            &base_args(&[
                "--allowed-origin",
                "https://UI.example.app/,https://ui.example.app:443",
                "--allowed-origin",
                "http://[0:0:0:0:0:0:0:1]:8080",
            ]),
            ServeEnv::default(),
        )?;
        assert_eq!(
            opts.allowed_origins,
            ["https://ui.example.app", "http://[::1]:8080"],
            "case, trailing slash, default port, and IPv6 spelling are canonical, with duplicates dropped"
        );
        Ok(())
    }

    #[test]
    fn allowed_origins_that_could_never_match_are_refused() {
        for bad in [
            "*",
            "ui.example.app",
            "ftp://ui.example.app",
            "https://u:p@ui.example.app",
            "https://ui.example.app/app",
            "https://ui.example.app/?x=1",
            "https://ui.example.app/#top",
            "https://ui.example.app?x=1",
            "https://ui.example.app#top",
        ] {
            let err = parse_serve_with(&base_args(&["--allowed-origin", bad]), ServeEnv::default())
                .err()
                .unwrap_or_default();
            assert!(
                err.starts_with("--allowed-origin must list "),
                "{bad:?} must be refused, got {err:?}"
            );
        }
    }
    /// A password that must never appear in any message below.
    const SECRET: &str = "hunter2-secret";

    #[test]
    fn origin_errors_never_quote_the_value_even_when_an_earlier_check_fails() {
        // Each fails before the credentials check: scheme, port, host, form.
        let inputs = [
            format!("ftp://user:{SECRET}@host.example"),
            format!("https://user:{SECRET}@host.example:notaport"),
            format!("https://user:{SECRET}@[not-an-ip]"),
            format!("https://user:{SECRET}@"),
            format!("//user:{SECRET}@host.example"),
            format!("user:{SECRET}@host.example"),
            format!("https://user:{SECRET}@host.example"),
            format!("https://user:{SECRET}@host.example/path?x=1"),
        ];
        for input in &inputs {
            for flag in ["--content-origin", "--api-origin", "--allowed-origin"] {
                let from_flag = parse_serve(&base_args(&[flag, input]))
                    .err()
                    .unwrap_or_default();
                assert!(
                    !from_flag.is_empty() && !from_flag.contains(SECRET),
                    "{flag} {input:?} must be refused without the secret, got {from_flag:?}"
                );
            }
            let env_cases = [
                serve_env(Some(input), None),
                ServeEnv {
                    api_origin: Some(input.clone()),
                    ..ServeEnv::default()
                },
                ServeEnv {
                    allowed_origins: Some(input.clone()),
                    ..ServeEnv::default()
                },
            ];
            for env in env_cases {
                let from_env = parse_serve_with(&base_args(&[]), env)
                    .err()
                    .unwrap_or_default();
                assert!(
                    !from_env.is_empty() && !from_env.contains(SECRET),
                    "{input:?} from the environment must be refused without the secret, got {from_env:?}"
                );
            }
        }
    }

    #[test]
    fn other_settings_do_not_quote_what_they_refuse() {
        let sensitive = format!("https://user:{SECRET}@host.example");
        let injecting = format!("{sensitive};x");
        let bad_prefix = format!("10.0.0.0/{SECRET}");
        let with_equals = format!("--content-origin={sensitive}");
        let cases: [(&str, Vec<&str>); 5] = [
            ("frame-ancestors", vec!["--frame-ancestors", &injecting]),
            ("lan", vec!["--lan", &sensitive]),
            ("trusted-proxy", vec!["--trusted-proxy", &sensitive]),
            ("trusted-proxy prefix", vec!["--trusted-proxy", &bad_prefix]),
            ("option with value", vec![&with_equals]),
        ];
        for (name, extra) in cases {
            let error = parse_serve(&base_args(&extra)).err().unwrap_or_default();
            assert!(
                !error.is_empty() && !error.contains(SECRET),
                "{name}: must be refused without the secret, got {error:?}"
            );
        }
        let stray = parse_serve(&base_args(&[&sensitive]))
            .err()
            .unwrap_or_default();
        assert!(
            stray.starts_with("Unknown arguments.") && !stray.contains(SECRET),
            "a stray value is not quoted: {stray:?}"
        );
        // A flag and its value in one argument are not an option name, however
        // they are joined, and no part of them is shown.
        for joined in [
            format!("--content-origin {sensitive}"),
            format!("--content-origin:{sensitive}"),
            format!("--content-origin\t{sensitive}"),
            format!("--Content-Origin={sensitive}"),
            format!("-{SECRET}"),
            format!("--content-origin\u{a0}{sensitive}"),
            SECRET.to_owned(),
        ] {
            let error = parse_serve(&base_args(&[&joined]))
                .err()
                .unwrap_or_default();
            assert!(
                error.starts_with("Unknown arguments.") && !error.contains(SECRET),
                "{joined:?} must not be quoted, got {error:?}"
            );
            assert!(
                !error.contains("content-origin") || joined.contains('='),
                "{joined:?}: only a name followed by '=' is named, got {error:?}"
            );
        }
        let equals = parse_serve(&base_args(&[&with_equals]))
            .err()
            .unwrap_or_default();
        assert!(
            equals.contains("--content-origin") && !equals.contains(SECRET),
            "the option name is kept, its value is not: {equals:?}"
        );
    }

    #[test]
    fn frame_ancestors_errors_do_not_show_the_character_that_was_refused() {
        // A refused character may belong to a password, so it is not printed.
        for bad in ['\u{20ac}', '\u{1}', '\u{e9}', '\u{7f}'] {
            let value = format!("https://a.example {bad}");
            let from_flag = parse_serve(&base_args(&["--frame-ancestors", &value]))
                .err()
                .unwrap_or_default();
            let from_env = parse_serve_with(&base_args(&[]), serve_env(None, Some(&value)))
                .err()
                .unwrap_or_default();
            for error in [from_flag, from_env] {
                assert!(
                    error.contains("visible ASCII")
                        && !error.contains(bad)
                        && !error.contains(&bad.escape_unicode().to_string())
                        && !error.contains(&format!("{bad:?}")),
                    "{bad:?} must not appear in {error:?}"
                );
            }
        }
    }

    #[test]
    fn network_settings_name_the_flag_or_variable_in_effect() {
        let bad = "not-an-address";
        let env = |lan: Option<&str>, loopback: Option<&str>, proxies: Option<&str>| ServeEnv {
            lan: lan.map(str::to_owned),
            loopback_listen: loopback.map(str::to_owned),
            trusted_proxies: proxies.map(str::to_owned),
            ..ServeEnv::default()
        };
        let refused = |args: &[&str], env: ServeEnv| {
            parse_serve_with(&base_args(args), env)
                .err()
                .unwrap_or_default()
        };
        assert!(refused(&["--lan", bad], ServeEnv::default()).starts_with("--lan must"));
        assert!(refused(&[], env(Some(bad), None, None)).starts_with("ARTIFREE_LAN must"));
        assert!(
            refused(&["--lan", "0.0.0.0"], ServeEnv::default()).starts_with("--lan must"),
            "unspecified address from the flag"
        );
        assert!(
            refused(&[], env(Some("0.0.0.0"), None, None)).starts_with("ARTIFREE_LAN must"),
            "unspecified address from the variable"
        );
        assert!(
            refused(&["--loopback-listen", bad], ServeEnv::default())
                .starts_with("--loopback-listen must")
        );
        assert!(
            refused(&[], env(None, Some(bad), None)).starts_with("ARTIFREE_LOOPBACK_LISTEN must")
        );
        assert!(
            refused(&[], env(None, Some("10.0.0.1:7410"), None))
                .starts_with("ARTIFREE_LOOPBACK_LISTEN must be a loopback")
        );
        assert!(
            refused(&["--trusted-proxy", bad], ServeEnv::default())
                .starts_with("--trusted-proxy lists")
        );
        assert!(
            refused(&[], env(None, None, Some("10.0.0.0/8,not-an-address")))
                .starts_with("ARTIFREE_TRUSTED_PROXIES lists")
        );
        // A flag in effect hides the variable, so it is the flag that is named.
        assert!(
            refused(&["--lan", bad], env(Some("192.168.1.2"), None, None))
                .starts_with("--lan must")
        );
    }

    #[test]
    fn older_variables_come_from_the_given_environment_only() -> Result<(), String> {
        // Whatever the real environment holds, parsing sees only this table.
        let env = ServeEnv {
            api_origin: Some("https://api.example.test".to_owned()),
            allowed_origins: Some("https://ui.example.test".to_owned()),
            allowed_hosts: Some("Host.example.test".to_owned()),
            trusted_proxies: Some("10.1.0.0/16".to_owned()),
            allow_remote_mcp: Some("1".to_owned()),
            lan: Some("192.168.1.50".to_owned()),
            web_dir: Some("/srv/web".to_owned()),
            ..ServeEnv::default()
        };
        let opts = parse_serve_with(&base_args(&[]), env)?;
        assert_eq!(opts.api_origin.as_deref(), Some("https://api.example.test"));
        assert!(
            opts.allowed_origins
                .contains(&"https://ui.example.test".to_owned())
        );
        assert!(opts.allowed_hosts.contains(&"Host.example.test".to_owned()));
        assert_eq!(opts.trusted_proxies.len(), 1);
        assert!(opts.allow_remote_mcp);
        assert_eq!(opts.api_address.to_string(), "192.168.1.50:7410");
        assert_eq!(opts.web_dir, Some(PathBuf::from("/srv/web")));

        let empty = parse_serve_with(&base_args(&[]), ServeEnv::default())?;
        assert_eq!(empty.api_origin, None);
        assert!(empty.allowed_origins.is_empty());
        assert!(!empty.allow_remote_mcp);
        assert_eq!(empty.web_dir, None);
        Ok(())
    }

    #[test]
    fn an_unusable_variable_matters_only_when_no_flag_overrides_it() -> Result<(), String> {
        let unusable = || ServeEnv {
            content_origin: Some(Err("ARTIFREE_CONTENT_ORIGIN is not valid UTF-8.".to_owned())),
            frame_ancestors: Some(Err(
                "ARTIFREE_FRAME_ANCESTORS is not valid UTF-8.".to_owned()
            )),
            ..ServeEnv::default()
        };
        let opts = parse_serve_with(
            &base_args(&[
                "--content-origin",
                "https://flag.example.com",
                "--frame-ancestors",
                "https://flag.example.app",
            ]),
            unusable(),
        )?;
        assert_eq!(
            opts.content_origin.as_deref(),
            Some("https://flag.example.com")
        );
        assert_eq!(opts.frame_ancestors, "https://flag.example.app");

        let origin_only = parse_serve_with(
            &base_args(&["--frame-ancestors", "https://flag.example.app"]),
            unusable(),
        )
        .err()
        .unwrap_or_default();
        assert_eq!(origin_only, "ARTIFREE_CONTENT_ORIGIN is not valid UTF-8.");
        let ancestors_only = parse_serve_with(
            &base_args(&["--content-origin", "https://flag.example.com"]),
            unusable(),
        )
        .err()
        .unwrap_or_default();
        assert_eq!(
            ancestors_only,
            "ARTIFREE_FRAME_ANCESTORS is not valid UTF-8."
        );
        Ok(())
    }

    #[test]
    fn equal_origins_name_where_each_value_came_from() {
        let from_env = parse_serve_with(
            &base_args(&[]),
            ServeEnv {
                api_origin: Some("https://same.example".to_owned()),
                ..serve_env(Some("https://same.example"), None)
            },
        )
        .err()
        .unwrap_or_default();
        assert_eq!(
            from_env,
            "ARTIFREE_CONTENT_ORIGIN must differ from ARTIFREE_API_ORIGIN to maintain preview sandbox isolation"
        );
        let mixed = parse_serve_with(
            &base_args(&["--api-origin", "https://same.example"]),
            serve_env(Some("https://same.example"), None),
        )
        .err()
        .unwrap_or_default();
        assert!(
            mixed.starts_with("ARTIFREE_CONTENT_ORIGIN must differ from --api-origin"),
            "{mixed}"
        );
        let bad_env_api = parse_serve_with(
            &base_args(&[]),
            ServeEnv {
                api_origin: Some("ftp://x.example".to_owned()),
                ..ServeEnv::default()
            },
        )
        .err()
        .unwrap_or_default();
        assert!(
            bad_env_api.starts_with("ARTIFREE_API_ORIGIN must be a valid HTTP or HTTPS origin"),
            "{bad_env_api}"
        );
    }
}
