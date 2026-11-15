//! A small blocking client for the NUT network protocol spoken by upsd
//! (TCP 3493): listing, descriptions, login, instant commands and SET VAR,
//! with optional result tracking. It uses `std::net` only; run it on a
//! thread of its own next to an async runtime.
//!
//! Reference: <https://networkupstools.org/docs/developer-guide.chunked/net-protocol.html>

use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// The port upsd listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 3493;

/// What went wrong talking to upsd.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The connection failed, broke or timed out.
    Io(io::Error),
    /// `ERR <code> [detail]` from upsd.
    Server {
        /// The error code, e.g. `ACCESS-DENIED` or `DATA-STALE`.
        code: String,
        /// Anything upsd said after the code.
        detail: Option<String>,
    },
    /// upsd said something we did not expect.
    Protocol(String),
    /// An argument the protocol cannot carry, such as one with a line
    /// break; nothing was sent.
    Argument(String),
}

impl Error {
    /// The upsd error code, for [`Error::Server`].
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Server { code, .. } => Some(code),
            _ => None,
        }
    }

    /// True when the connection itself is gone and has to be reopened.
    pub fn is_fatal(&self) -> bool {
        matches!(self, Error::Io(_) | Error::Protocol(_))
    }

    /// Errors that mean the credentials (or their permissions) are wrong.
    pub fn is_auth(&self) -> bool {
        matches!(
            self.code(),
            Some("ACCESS-DENIED" | "INVALID-PASSWORD" | "INVALID-USERNAME" | "USERNAME-REQUIRED" | "PASSWORD-REQUIRED")
        )
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Human wording for an upsd error code, or an empty string for a code
/// this crate does not know.
pub fn describe_code(code: &str) -> &'static str {
    match code {
        "ACCESS-DENIED" => "access denied (check the user's permissions in upsd.users)",
        "UNKNOWN-UPS" => "no such UPS on this server",
        "VAR-NOT-SUPPORTED" => "variable not supported",
        "CMD-NOT-SUPPORTED" => "command not supported",
        "INVALID-ARGUMENT" => "invalid argument",
        "INSTCMD-FAILED" => "the driver could not run the command",
        "SET-FAILED" => "the driver could not set the variable",
        "READONLY" => "variable is read-only",
        "TOO-LONG" => "value is too long",
        "FEATURE-NOT-SUPPORTED" => "feature not supported by this server",
        "FEATURE-NOT-CONFIGURED" => "feature not configured on this server",
        "DRIVER-NOT-CONNECTED" => "the driver is not running or not connected to upsd",
        "DATA-STALE" => "data is stale (the driver lost contact with the UPS)",
        "ALREADY-LOGGED-IN" => "already logged in",
        "INVALID-PASSWORD" => "invalid password",
        "ALREADY-SET-PASSWORD" => "password already set on this connection",
        "INVALID-USERNAME" => "invalid username",
        "ALREADY-SET-USERNAME" => "username already set on this connection",
        "USERNAME-REQUIRED" => "a username is required",
        "PASSWORD-REQUIRED" => "a password is required",
        "UNKNOWN-COMMAND" => "upsd does not know this command",
        "INVALID-VALUE" => "invalid value",
        _ => "",
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => match e.kind() {
                io::ErrorKind::ConnectionRefused => write!(f, "connection refused"),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => write!(f, "timed out"),
                io::ErrorKind::UnexpectedEof => write!(f, "upsd closed the connection"),
                _ => write!(f, "{e}"),
            },
            Error::Server { code, detail } => {
                let text = describe_code(code);
                match (text.is_empty(), detail) {
                    (true, Some(d)) => write!(f, "{code} {d}"),
                    (true, None) => write!(f, "{code}"),
                    (false, Some(d)) => write!(f, "{text} ({d})"),
                    (false, None) => write!(f, "{text}"),
                }
            }
            Error::Protocol(msg) => write!(f, "protocol error: {msg}"),
            Error::Argument(msg) => write!(f, "invalid argument: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// A result with an upsd [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Split a protocol line into words; double-quoted words may contain
/// spaces, `\"` and `\\`.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else { break };
        let mut word = String::new();
        if first == '"' {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        if let Some(n) = chars.next() {
                            word.push(n);
                        }
                    }
                    '"' => break,
                    _ => word.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                word.push(c);
                chars.next();
            }
        }
        out.push(word);
    }
    out
}

/// Quote an argument for sending when it is not a plain word.
pub fn quote(arg: &str) -> String {
    let plain = !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "._-:/@+".contains(c));
    if plain {
        return arg.to_string();
    }
    let mut s = String::with_capacity(arg.len() + 2);
    s.push('"');
    for c in arg.chars() {
        if c == '"' || c == '\\' {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('"');
    s
}

fn parse_err(line: &str) -> Option<Error> {
    let rest = line.strip_prefix("ERR")?;
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let mut parts = rest.trim().splitn(2, ' ');
    let code = parts.next().unwrap_or("").to_string();
    let detail = parts.next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Some(Error::Server { code, detail })
}

/// Result of polling a tracked instant command or SET VAR.
#[derive(Debug)]
pub enum Tracking {
    /// The driver has not answered yet.
    Pending,
    /// It worked.
    Success,
    /// upsd or the driver refused.
    Failed(Error),
}

/// What upsd says about a variable, see [`Client::var_info`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VarInfo {
    /// The description, when upsd has one.
    pub desc: Option<String>,
    /// Words from `GET TYPE`, e.g. `RW`, `ENUM`, `STRING:32`, `NUMBER`.
    pub kind: Vec<String>,
    /// The values an `ENUM` variable can take.
    pub enum_values: Vec<String>,
    /// The (low, high) ranges a `RANGE` variable can take.
    pub ranges: Vec<(String, String)>,
}

impl VarInfo {
    /// The longest value a `STRING:n` variable takes.
    pub fn max_len(&self) -> Option<usize> {
        self.kind.iter().find_map(|k| k.strip_prefix("STRING:")?.parse().ok())
    }
}

/// An instant command and its description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmdInfo {
    /// The command, e.g. `test.battery.start.quick`.
    pub name: String,
    /// The description, when upsd has one.
    pub desc: Option<String>,
}

/// Commands that can cut power to the load or deep-cycle the battery;
/// upst asks for the command to be typed out before it runs one.
///
/// ```
/// use upst_core::nut::dangerous;
///
/// assert!(dangerous("load.off") && dangerous("shutdown.return"));
/// assert!(!dangerous("beeper.mute"));
/// ```
pub fn dangerous(cmd: &str) -> bool {
    cmd.starts_with("shutdown.")
        || cmd.contains("load.off")
        || cmd.contains("killpower")
        || cmd.contains("reboot")
        || cmd.starts_with("calibrate.start")
        || cmd.starts_with("test.battery.start.deep")
        || cmd.starts_with("bypass.start")
}

/// A connection to upsd. Every call blocks until upsd answers or the
/// timeout given to [`Client::connect`] runs out; after an error for
/// which [`Error::is_fatal`] is true, connect again.
///
/// The client keeps no credentials: [`Client::authenticate`] sends them
/// and forgets them.
pub struct Client {
    reader: BufReader<TcpStream>,
    stream: TcpStream,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").field("peer", &self.stream.peer_addr().ok()).finish_non_exhaustive()
    }
}

impl Client {
    /// Connect to upsd. `timeout` applies to connecting and to every
    /// read and write after that; zero means none.
    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<Client> {
        let timeout = (!timeout.is_zero()).then_some(timeout);
        let mut last = None;
        for addr in (host, port).to_socket_addrs()? {
            let conn = match timeout {
                Some(t) => TcpStream::connect_timeout(&addr, t),
                None => TcpStream::connect(addr),
            };
            match conn {
                Ok(stream) => {
                    stream.set_read_timeout(timeout)?;
                    stream.set_write_timeout(timeout)?;
                    let _ = stream.set_nodelay(true);
                    let reader = BufReader::new(stream.try_clone()?);
                    return Ok(Client { reader, stream });
                }
                Err(e) => last = Some(e),
            }
        }
        Err(match last {
            Some(e) => Error::Io(e),
            None => Error::Protocol(format!("{host} did not resolve to any address")),
        })
    }

    fn send(&mut self, line: &str) -> Result<()> {
        // A line break would end the request early and start another one.
        if line.contains(['\n', '\r']) {
            return Err(Error::Argument("line break in an argument".into()));
        }
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        self.stream.write_all(&buf)?;
        Ok(())
    }

    fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(Error::Io(io::ErrorKind::UnexpectedEof.into()));
        }
        while line.ends_with('\n') || line.ends_with('\r') {
            line.pop();
        }
        Ok(line)
    }

    /// Send one command and read its single-line answer.
    fn request(&mut self, cmd: &str) -> Result<String> {
        self.send(cmd)?;
        let line = self.read_line()?;
        match parse_err(&line) {
            Some(e) => Err(e),
            None => Ok(line),
        }
    }

    /// `LIST <query>`; returns the tokenized lines between BEGIN and END.
    fn list(&mut self, query: &str) -> Result<Vec<Vec<String>>> {
        let first = self.request(&format!("LIST {query}"))?;
        if !first.starts_with("BEGIN LIST") {
            return Err(Error::Protocol(format!("expected BEGIN LIST, got {first:?}")));
        }
        let mut rows = Vec::new();
        loop {
            let line = self.read_line()?;
            if line.starts_with("END LIST") {
                return Ok(rows);
            }
            if let Some(e) = parse_err(&line) {
                return Err(e);
            }
            rows.push(tokenize(&line));
        }
    }

    /// Every UPS on the server with its description (`LIST UPS`).
    pub fn list_ups(&mut self) -> Result<Vec<(String, String)>> {
        Ok(self
            .list("UPS")?
            .into_iter()
            .filter(|r| r.len() >= 2 && r[0] == "UPS")
            .map(|mut r| {
                let desc = if r.len() > 2 { r.swap_remove(2) } else { String::new() };
                (r.swap_remove(1), desc)
            })
            .collect())
    }

    fn pairs(&mut self, kind: &str, ups: &str) -> Result<Vec<(String, String)>> {
        Ok(self
            .list(&format!("{kind} {}", quote(ups)))?
            .into_iter()
            .filter(|r| r.len() >= 4 && r[0] == kind)
            .map(|mut r| {
                let value = r.swap_remove(3);
                (r.swap_remove(2), value)
            })
            .collect())
    }

    /// Every variable of a UPS and its value (`LIST VAR`).
    pub fn list_vars(&mut self, ups: &str) -> Result<Vec<(String, String)>> {
        self.pairs("VAR", ups)
    }

    /// The writable variables of a UPS and their values (`LIST RW`).
    pub fn list_rw(&mut self, ups: &str) -> Result<Vec<(String, String)>> {
        self.pairs("RW", ups)
    }

    /// The instant commands a UPS supports (`LIST CMD`).
    pub fn list_commands(&mut self, ups: &str) -> Result<Vec<String>> {
        Ok(self
            .list(&format!("CMD {}", quote(ups)))?
            .into_iter()
            .filter(|r| r.len() >= 3 && r[0] == "CMD")
            .map(|mut r| r.swap_remove(2))
            .collect())
    }

    /// The values an `ENUM` variable can take (`LIST ENUM`).
    pub fn list_enum(&mut self, ups: &str, var: &str) -> Result<Vec<String>> {
        Ok(self
            .list(&format!("ENUM {} {}", quote(ups), quote(var)))?
            .into_iter()
            .filter(|r| r.len() >= 4 && r[0] == "ENUM")
            .map(|mut r| r.swap_remove(3))
            .collect())
    }

    /// The (low, high) ranges a `RANGE` variable can take (`LIST RANGE`).
    pub fn list_range(&mut self, ups: &str, var: &str) -> Result<Vec<(String, String)>> {
        Ok(self
            .list(&format!("RANGE {} {}", quote(ups), quote(var)))?
            .into_iter()
            .filter(|r| r.len() >= 5 && r[0] == "RANGE")
            .map(|r| (r[3].clone(), r[4].clone()))
            .collect())
    }

    /// The last word of a `GET` answer, or `None` for upsd's placeholder.
    fn get_text(&mut self, query: &str, skip: usize) -> Result<Option<String>> {
        let line = self.request(&format!("GET {query}"))?;
        let text = tokenize(&line).into_iter().nth(skip).unwrap_or_default();
        Ok(Some(text).filter(|t| !t.is_empty() && t != "Description unavailable"))
    }

    /// A variable's description (`GET DESC`), or `None` when upsd has
    /// none.
    pub fn var_description(&mut self, ups: &str, var: &str) -> Result<Option<String>> {
        self.get_text(&format!("DESC {} {}", quote(ups), quote(var)), 3)
    }

    /// An instant command's description (`GET CMDDESC`), or `None` when
    /// upsd has none.
    pub fn command_description(&mut self, ups: &str, cmd: &str) -> Result<Option<String>> {
        self.get_text(&format!("CMDDESC {} {}", quote(ups), quote(cmd)), 3)
    }

    /// Words after `TYPE <ups> <var>`, e.g. `["RW", "STRING:32"]`.
    pub fn var_type(&mut self, ups: &str, var: &str) -> Result<Vec<String>> {
        let line = self.request(&format!("GET TYPE {} {}", quote(ups), quote(var)))?;
        Ok(tokenize(&line).into_iter().skip(3).collect())
    }

    /// Everything upsd says about a variable: its description, type, and
    /// the values it can take. A failing `GET DESC` or a broken connection
    /// is an error; anything else upsd refuses is left out.
    pub fn var_info(&mut self, ups: &str, var: &str) -> Result<VarInfo> {
        fn soft<T: Default>(r: Result<T>) -> Result<T> {
            match r {
                Err(e) if e.is_fatal() => Err(e),
                r => Ok(r.unwrap_or_default()),
            }
        }
        let mut info = VarInfo { desc: self.var_description(ups, var)?, ..VarInfo::default() };
        info.kind = soft(self.var_type(ups, var))?;
        if info.kind.iter().any(|k| k == "ENUM") {
            info.enum_values = soft(self.list_enum(ups, var))?;
        }
        if info.kind.iter().any(|k| k == "RANGE") {
            info.ranges = soft(self.list_range(ups, var))?;
        }
        Ok(info)
    }

    /// The server banner, e.g. `Network UPS Tools upsd 2.8.4 - https://...`.
    pub fn server_version(&mut self) -> Result<String> {
        self.request("VER")
    }

    /// Log in (`USERNAME` and `PASSWORD`), as needed for commands and
    /// SET VAR.
    pub fn authenticate(&mut self, user: &str, password: &str) -> Result<()> {
        self.request(&format!("USERNAME {}", quote(user)))?;
        self.request(&format!("PASSWORD {}", quote(password)))?;
        Ok(())
    }

    /// Ask upsd to track command results (NUT 2.8+). Needs a username.
    pub fn enable_tracking(&mut self) -> bool {
        self.request("SET TRACKING ON").is_ok()
    }

    /// Run an instant command, with an optional value. Returns the
    /// tracking id when tracking is on, see [`Client::tracking`].
    pub fn instcmd(&mut self, ups: &str, cmd: &str, value: Option<&str>) -> Result<Option<String>> {
        let mut line = format!("INSTCMD {} {}", quote(ups), quote(cmd));
        if let Some(v) = value {
            line.push(' ');
            line.push_str(&quote(v));
        }
        let reply = self.request(&line)?;
        Ok(tracking_id(&reply))
    }

    /// Set a writable variable. Returns the tracking id when tracking is
    /// on, see [`Client::tracking`].
    pub fn set_var(&mut self, ups: &str, var: &str, value: &str) -> Result<Option<String>> {
        // upsd wants the value quoted even when it is a plain word.
        let mut quoted = quote(value);
        if !quoted.starts_with('"') {
            quoted = format!("\"{quoted}\"");
        }
        let reply = self.request(&format!("SET VAR {} {} {quoted}", quote(ups), quote(var)))?;
        Ok(tracking_id(&reply))
    }

    /// How a tracked command or SET VAR went (`GET TRACKING`).
    pub fn tracking(&mut self, id: &str) -> Result<Tracking> {
        match self.request(&format!("GET TRACKING {}", quote(id))) {
            Ok(line) if line.trim() == "PENDING" => Ok(Tracking::Pending),
            Ok(line) if line.trim() == "SUCCESS" => Ok(Tracking::Success),
            Ok(line) => Err(Error::Protocol(format!("unexpected tracking reply {line:?}"))),
            Err(e) if e.is_fatal() => Err(e),
            Err(e) => Ok(Tracking::Failed(e)),
        }
    }

    /// Say goodbye and close the connection.
    pub fn logout(mut self) {
        let _ = self.request("LOGOUT");
    }
}

fn tracking_id(reply: &str) -> Option<String> {
    reply.strip_prefix("OK TRACKING ").map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_quotes_and_escapes() {
        assert_eq!(
            tokenize(r#"VAR ups ups.model "Smart \"UPS\" 1500""#),
            ["VAR", "ups", "ups.model", r#"Smart "UPS" 1500"#]
        );
        assert_eq!(tokenize(r#"VAR ups battery.type """#), ["VAR", "ups", "battery.type", ""]);
        assert_eq!(tokenize("  a   b "), ["a", "b"]);
        assert_eq!(tokenize(r#""a\\b""#), [r"a\b"]);
    }

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(quote("battery.charge"), "battery.charge");
        assert_eq!(quote("two words"), "\"two words\"");
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(quote(""), "\"\"");
    }

    #[test]
    fn parses_errors() {
        let e = parse_err("ERR DATA-STALE").unwrap();
        assert_eq!(e.code(), Some("DATA-STALE"));
        let e = parse_err("ERR INVALID-ARGUMENT some detail").unwrap();
        assert!(matches!(e, Error::Server { ref detail, .. } if detail.as_deref() == Some("some detail")));
        assert!(parse_err("ERROR").is_none());
        assert!(parse_err("OK").is_none());
    }
}
