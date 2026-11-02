//! The environment of `artifree-server serve`, read once (decision 0028).
//!
//! Every `ARTIFREE_*` variable that `serve` understands is copied from the
//! process into a [`ServeEnv`] when the command starts. The option parser only
//! ever sees the struct, so tests build one by hand and never depend on, or
//! change, the process environment.

use std::collections::BTreeMap;
use std::ffi::OsString;

/// `ARTIFREE_WEB_DIR`: directory of production web assets.
pub const WEB_DIR: &str = "ARTIFREE_WEB_DIR";
/// `ARTIFREE_LAN`: LAN IP address to bind.
pub const LAN: &str = "ARTIFREE_LAN";
/// `ARTIFREE_LOOPBACK_LISTEN`: dedicated loopback API address.
pub const LOOPBACK_LISTEN: &str = "ARTIFREE_LOOPBACK_LISTEN";
/// `ARTIFREE_CONTENT_ORIGIN`: public preview origin.
pub const CONTENT_ORIGIN: &str = "ARTIFREE_CONTENT_ORIGIN";
/// `ARTIFREE_API_ORIGIN`: public API and web origin.
pub const API_ORIGIN: &str = "ARTIFREE_API_ORIGIN";
/// `ARTIFREE_TRUSTED_PROXIES`: comma-separated trusted proxy networks.
pub const TRUSTED_PROXIES: &str = "ARTIFREE_TRUSTED_PROXIES";
/// `ARTIFREE_ALLOWED_HOSTS`: comma-separated permitted `Host` values.
pub const ALLOWED_HOSTS: &str = "ARTIFREE_ALLOWED_HOSTS";
/// `ARTIFREE_ALLOWED_ORIGINS`: comma-separated permitted CORS origins.
pub const ALLOWED_ORIGINS: &str = "ARTIFREE_ALLOWED_ORIGINS";
/// `ARTIFREE_ALLOW_REMOTE_MCP`: `true` or `1` permits remote MCP.
pub const ALLOW_REMOTE_MCP: &str = "ARTIFREE_ALLOW_REMOTE_MCP";
/// `ARTIFREE_FRAME_ANCESTORS`: CSP `frame-ancestors` source list.
pub const FRAME_ANCESTORS: &str = "ARTIFREE_FRAME_ANCESTORS";

const NAMES: [&str; 10] = [
    WEB_DIR,
    LAN,
    LOOPBACK_LISTEN,
    CONTENT_ORIGIN,
    API_ORIGIN,
    TRUSTED_PROXIES,
    ALLOWED_HOSTS,
    ALLOWED_ORIGINS,
    ALLOW_REMOTE_MCP,
    FRAME_ANCESTORS,
];

/// A setting's text together with the name of the flag or variable that
/// supplied it. Errors name the source and never repeat the text.
#[derive(Debug, Clone, Copy)]
pub struct Given<'a> {
    /// The flag (`--lan`) or variable (`ARTIFREE_LAN`) in effect.
    pub name: &'static str,
    /// The operator's text. Do not put it in a message.
    pub text: &'a str,
}

/// The `serve` environment variables, as read from the process once.
#[derive(Debug, Clone, Default)]
pub struct ServeEnv {
    values: BTreeMap<&'static str, OsString>,
}

impl ServeEnv {
    /// Reads every variable `serve` understands from the process.
    #[must_use]
    pub fn from_process() -> Self {
        let values = NAMES
            .into_iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
            .collect();
        Self { values }
    }

    /// An environment with one more variable set; tests build theirs this way.
    ///
    /// # Panics
    ///
    /// When `name` is not a variable `serve` reads, so that a misspelt name in
    /// a test cannot pass by being ignored.
    #[cfg(test)]
    #[must_use]
    pub fn with(mut self, name: &'static str, value: impl Into<OsString>) -> Self {
        assert!(NAMES.contains(&name), "{name} is not a serve variable");
        self.values.insert(name, value.into());
        self
    }

    /// The text of an older variable. A variable that is empty or not valid
    /// UTF-8 counts as unset, as it did before decision 0028.
    #[must_use]
    pub fn text(&self, name: &str) -> Option<&str> {
        self.values
            .get(name)
            .and_then(|value| value.to_str())
            .filter(|text| !text.is_empty())
    }

    /// A setting where the flag wins over an older variable. A flag counts
    /// even when its text is empty; an empty or non-UTF-8 variable is unset.
    #[must_use]
    pub fn lenient<'a>(
        &'a self,
        flag_name: &'static str,
        flag: Option<&'a str>,
        variable: &'static str,
    ) -> Option<Given<'a>> {
        match flag {
            Some(text) => Some(Given {
                name: flag_name,
                text,
            }),
            None => self.text(variable).map(|text| Given {
                name: variable,
                text,
            }),
        }
    }

    /// A setting where the flag wins over a variable, and a variable that is
    /// set but not valid UTF-8 is an error only when no flag overrides it. An
    /// empty variable is unset.
    ///
    /// # Errors
    ///
    /// When there is no flag and the variable is not valid UTF-8.
    pub fn strict<'a>(
        &'a self,
        flag_name: &'static str,
        flag: Option<&'a str>,
        variable: &'static str,
    ) -> Result<Option<Given<'a>>, String> {
        if let Some(text) = flag {
            return Ok(Some(Given {
                name: flag_name,
                text,
            }));
        }
        match self.values.get(variable) {
            None => Ok(None),
            Some(value) => match value.to_str() {
                Some("") => Ok(None),
                Some(text) => Ok(Some(Given {
                    name: variable,
                    text,
                })),
                None => Err(format!(
                    "{variable} is not valid UTF-8; fix it or pass {flag_name}."
                )),
            },
        }
    }

    /// The comma-separated entries of repeated flags, then of an older
    /// variable. Entries are trimmed and empty ones are skipped.
    #[must_use]
    pub fn entries<'a>(
        &'a self,
        flag_name: &'static str,
        flags: &'a [String],
        variable: &'static str,
    ) -> Vec<Given<'a>> {
        let from_flags = flags.iter().map(|text| (flag_name, text.as_str()));
        let from_variable = self.text(variable).into_iter().map(|text| (variable, text));
        from_flags
            .chain(from_variable)
            .flat_map(|(name, list)| {
                list.split(',').map(move |part| Given {
                    name,
                    text: part.trim(),
                })
            })
            .filter(|given| !given.text.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_wins_even_when_empty_and_an_empty_variable_is_unset() {
        let env = ServeEnv::default().with(LAN, "10.0.0.1");
        let flag = env
            .lenient("--lan", Some(""), LAN)
            .map(|g| (g.name, g.text));
        assert_eq!(flag, Some(("--lan", "")));
        let variable = env.lenient("--lan", None, LAN).map(|g| (g.name, g.text));
        assert_eq!(variable, Some((LAN, "10.0.0.1")));
        let empty = ServeEnv::default().with(LAN, "");
        assert!(empty.lenient("--lan", None, LAN).is_none());
        assert!(matches!(empty.strict("--lan", None, LAN), Ok(None)));
    }

    #[cfg(unix)]
    #[test]
    fn a_variable_that_is_not_utf8_fails_only_when_no_flag_overrides_it() {
        use std::os::unix::ffi::OsStringExt;
        let env = ServeEnv::default().with(CONTENT_ORIGIN, OsString::from_vec(vec![0x66, 0xff]));
        let flag = env
            .strict("--content-origin", Some("http://a.example"), CONTENT_ORIGIN)
            .map(|given| given.map(|given| given.text));
        assert_eq!(flag, Ok(Some("http://a.example")));
        let Err(error) = env.strict("--content-origin", None, CONTENT_ORIGIN) else {
            panic!("a non-UTF-8 variable without a flag must be an error");
        };
        assert!(error.contains(CONTENT_ORIGIN), "{error}");
        // The older variables keep treating such a value as unset.
        assert_eq!(
            ServeEnv::default()
                .with(ALLOWED_ORIGINS, OsString::from_vec(vec![0xff]))
                .text(ALLOWED_ORIGINS),
            None
        );
    }

    #[test]
    fn entries_come_from_flags_then_the_variable_without_blanks() {
        let env = ServeEnv::default().with(ALLOWED_HOSTS, " c.example , ,d.example");
        let flags = vec!["a.example, b.example".to_owned(), String::new()];
        let entries: Vec<(&str, &str)> = env
            .entries("--allowed-host", &flags, ALLOWED_HOSTS)
            .iter()
            .map(|g| (g.name, g.text))
            .collect();
        assert_eq!(
            entries,
            [
                ("--allowed-host", "a.example"),
                ("--allowed-host", "b.example"),
                (ALLOWED_HOSTS, "c.example"),
                (ALLOWED_HOSTS, "d.example"),
            ]
        );
    }
}
