//! `ups[@host[:port]]` target syntax, the same one upsc uses, plus
//! `@host[:port]` for "every UPS on that server".

use std::fmt;
use std::str::FromStr;

use crate::nut::DEFAULT_PORT;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Server {
    pub host: String,
    pub port: u16,
}

impl Server {
    pub fn localhost() -> Self {
        Server { host: "localhost".into(), port: DEFAULT_PORT }
    }
}

impl fmt::Display for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == DEFAULT_PORT { write!(f, "{host}") } else { write!(f, "{host}:{}", self.port) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// `None` means every UPS the server lists.
    pub ups: Option<String>,
    pub server: Server,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.ups.as_deref().unwrap_or(""), self.server)
    }
}

/// `host[:port]`, `[v6addr][:port]` or a bare IPv6 address.
pub fn parse_server(s: &str, default_port: u16) -> Result<Server, String> {
    let bad_port = |p: &str| format!("bad port {p:?}");
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or("missing ] after IPv6 address")?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| bad_port(p))?,
            None if after.is_empty() => default_port,
            None => return Err(format!("unexpected {after:?} after ]")),
        };
        (host.to_string(), port)
    } else if s.matches(':').count() == 1 {
        let (host, p) = s.split_once(':').unwrap();
        (host.to_string(), p.parse().map_err(|_| bad_port(p))?)
    } else {
        // No colon, or a bare IPv6 address.
        (s.to_string(), default_port)
    };
    let host = if host.is_empty() { "localhost".to_string() } else { host };
    Ok(Server { host, port })
}

impl FromStr for Target {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty target".into());
        }
        let (ups, server) = match s.split_once('@') {
            Some((ups, host)) => (ups, parse_server(host, DEFAULT_PORT)?),
            None => (s, Server::localhost()),
        };
        if ups.chars().any(|c| c.is_whitespace() || c == '"') {
            return Err(format!("bad UPS name {ups:?}"));
        }
        let ups = Some(ups.to_string()).filter(|u| !u.is_empty() && u != "*");
        Ok(Target { ups, server })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Target {
        s.parse().unwrap()
    }

    #[test]
    fn parses_upsc_syntax() {
        assert_eq!(t("ups"), Target { ups: Some("ups".into()), server: Server::localhost() });
        let x = t("rack@nas.lan:3500");
        assert_eq!(x.ups.as_deref(), Some("rack"));
        assert_eq!(x.server, Server { host: "nas.lan".into(), port: 3500 });
        assert_eq!(t("@nas").ups, None);
        assert_eq!(t("*@nas").ups, None);
        assert_eq!(t("@").server, Server::localhost());
    }

    #[test]
    fn parses_ipv6() {
        assert_eq!(t("ups@[::1]:3494").server, Server { host: "::1".into(), port: 3494 });
        assert_eq!(t("ups@::1").server, Server { host: "::1".into(), port: DEFAULT_PORT });
        assert_eq!(t("ups@[fe80::1]").server.to_string(), "[fe80::1]");
    }

    #[test]
    fn rejects_garbage() {
        assert!("".parse::<Target>().is_err());
        assert!("ups@host:http".parse::<Target>().is_err());
        assert!("a b@host".parse::<Target>().is_err());
    }
}
