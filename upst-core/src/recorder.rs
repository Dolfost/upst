//! A client for `upst record`, the service that keeps a [journal] of
//! every UPS it watches and serves it over a Unix socket or TCP.
//!
//! The protocol is line based, like upsd's:
//!
//! ```text
//! VER                      upst 0.1.0 journal 1
//! LIST                     BEGIN LIST / UPS <label> <first> <last> ... / END LIST
//! GET <label> [<since>]    BEGIN <label> / journal lines / END
//! QUIT                     OK Goodbye
//! ```
//!
//! Errors come back as `ERR <what>`. Times are Unix seconds.
//!
//! # Stability
//!
//! This is protocol version 1. It only grows: new commands, and new
//! fields at the end of a line. A client skips `LIST` lines it does not
//! understand.
//!
//! [journal]: crate::journal

use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use crate::journal::Record;
use crate::target::{Server, parse_server};

/// The TCP port a recorder listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 3494;

/// Where a recorder listens: a Unix socket path, or `host[:port]`.
///
/// ```
/// use upst_core::recorder::Addr;
///
/// assert!(matches!("/run/upst/upst.sock".parse(), Ok(Addr::Unix(_))));
/// assert_eq!("nas".parse::<Addr>().unwrap().to_string(), "nas:3494");
/// ```
#[derive(Clone, Debug, PartialEq)]
pub enum Addr {
    /// A Unix socket.
    Unix(PathBuf),
    /// A TCP host and port.
    Tcp(Server),
}

/// Anything with a `/` is a socket path; anything else is
/// `host[:port]`, with [`DEFAULT_PORT`] when no port is given.
impl FromStr for Addr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty address".into());
        }
        if s.contains('/') {
            return Ok(Addr::Unix(PathBuf::from(s)));
        }
        parse_server(s, DEFAULT_PORT).map(Addr::Tcp)
    }
}

impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Addr::Unix(p) => write!(f, "{}", p.display()),
            Addr::Tcp(s) if s.host.contains(':') => write!(f, "[{}]:{}", s.host, s.port),
            Addr::Tcp(s) => write!(f, "{}:{}", s.host, s.port),
        }
    }
}

/// Sockets to look for a recorder at when none is named: the system
/// service's (`/run/upst/upst.sock`), then a user service's
/// (`$XDG_RUNTIME_DIR/upst/upst.sock`).
pub fn default_sockets() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/run/upst/upst.sock")];
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        v.push(PathBuf::from(dir).join("upst/upst.sock"));
    }
    v
}

/// A connection over a Unix socket or TCP.
#[derive(Debug)]
pub enum Stream {
    /// A Unix socket connection.
    Unix(UnixStream),
    /// A TCP connection.
    Tcp(TcpStream),
}

impl Stream {
    /// Connect, with `timeout` for connecting over TCP and for every read
    /// and write after that; zero means none.
    pub fn connect(addr: &Addr, timeout: Duration) -> io::Result<Stream> {
        let timeout = (!timeout.is_zero()).then_some(timeout);
        let s = match addr {
            Addr::Unix(p) => Stream::Unix(UnixStream::connect(p)?),
            Addr::Tcp(srv) => {
                let mut last = None;
                let mut found = None;
                for a in (srv.host.as_str(), srv.port).to_socket_addrs()? {
                    let conn = match timeout {
                        Some(t) => TcpStream::connect_timeout(&a, t),
                        None => TcpStream::connect(a),
                    };
                    match conn {
                        Ok(s) => {
                            found = Some(s);
                            break;
                        }
                        Err(e) => last = Some(e),
                    }
                }
                match found {
                    Some(s) => Stream::Tcp(s),
                    None => return Err(last.unwrap_or_else(|| io::Error::other("no address"))),
                }
            }
        };
        s.timeouts(timeout)?;
        Ok(s)
    }

    /// Set the read and write timeouts.
    pub fn timeouts(&self, t: Option<Duration>) -> io::Result<()> {
        match self {
            Stream::Unix(s) => {
                s.set_read_timeout(t)?;
                s.set_write_timeout(t)
            }
            Stream::Tcp(s) => {
                s.set_read_timeout(t)?;
                s.set_write_timeout(t)
            }
        }
    }

    /// Another handle to the same connection.
    pub fn try_clone(&self) -> io::Result<Stream> {
        Ok(match self {
            Stream::Unix(s) => Stream::Unix(s.try_clone()?),
            Stream::Tcp(s) => Stream::Tcp(s.try_clone()?),
        })
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Unix(s) => s.read(buf),
            Stream::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Unix(s) => s.write(buf),
            Stream::Tcp(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Unix(s) => s.flush(),
            Stream::Tcp(s) => s.flush(),
        }
    }
}

/// A journal as `LIST` reports it.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Listed {
    /// The UPS label, e.g. `ups@localhost`, as the recorder names it.
    pub label: String,
    /// The time of its first record, in Unix seconds.
    pub first: f64,
    /// The time of its last record, in Unix seconds.
    pub last: f64,
}

/// A connection to a recorder.
///
/// ```no_run
/// use std::time::Duration;
/// use upst_core::recorder::{Addr, Client};
///
/// let addr: Addr = "/run/upst/upst.sock".parse().unwrap();
/// let mut c = Client::connect(&addr, Duration::from_secs(5))?;
/// for j in c.list()? {
///     let records = c.get(&j.label, j.last - 3600.0)?;
///     println!("{}: {} records in the last hour", j.label, records.len());
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug)]
pub struct Client {
    r: BufReader<Stream>,
    w: Stream,
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

impl Client {
    /// Connect, with `timeout` for connecting and for every answer.
    pub fn connect(addr: &Addr, timeout: Duration) -> io::Result<Client> {
        let w = Stream::connect(addr, timeout)?;
        Ok(Client { r: BufReader::new(w.try_clone()?), w })
    }

    fn send(&mut self, line: &str) -> io::Result<()> {
        // A line break would end the request early and start another one.
        if line.contains(['\n', '\r']) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "line break in a request"));
        }
        self.w.write_all(format!("{line}\n").as_bytes())
    }

    fn line(&mut self) -> io::Result<String> {
        let mut s = String::new();
        if self.r.read_line(&mut s)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the recorder closed the connection"));
        }
        Ok(s.trim_end_matches(['\n', '\r']).to_string())
    }

    fn begin(&mut self) -> io::Result<()> {
        let l = self.line()?;
        if l.starts_with("BEGIN ") {
            Ok(())
        } else if let Some(e) = l.strip_prefix("ERR ") {
            Err(io::Error::other(format!("the recorder says {e}")))
        } else {
            Err(bad(format!("unexpected reply {l:?}")))
        }
    }

    /// The recorder's banner, e.g. `upst 0.1.2 journal 1`. Anything that
    /// does not start with `upst ` is not a recorder.
    pub fn version(&mut self) -> io::Result<String> {
        self.send("VER")?;
        self.line()
    }

    /// The journals the recorder has.
    pub fn list(&mut self) -> io::Result<Vec<Listed>> {
        self.send("LIST")?;
        self.begin()?;
        let mut out = Vec::new();
        loop {
            let l = self.line()?;
            if l.starts_with("END") {
                return Ok(out);
            }
            let w: Vec<&str> = l.split(' ').collect();
            // Lines this client does not understand are skipped.
            if let ["UPS", label, first, last, ..] = w.as_slice()
                && let (Ok(first), Ok(last)) = (first.parse(), last.parse())
            {
                out.push(Listed { label: label.to_string(), first, last });
            }
        }
    }

    /// The journal of `label` from `since` (Unix seconds) on, opening
    /// with records that recreate the state just before it. Feed it to
    /// [`Ups::replay`](crate::Ups::replay).
    pub fn get(&mut self, label: &str, since: f64) -> io::Result<Vec<Record>> {
        // Labels are one word, as `list` gives them.
        if label.is_empty() || label.contains(char::is_whitespace) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("bad label {label:?}")));
        }
        self.send(&format!("GET {label} {}", since.floor() as i64))?;
        self.begin()?;
        let mut out = Vec::new();
        loop {
            let l = self.line()?;
            if l == "END" {
                return Ok(out);
            }
            if let Some(r) = Record::parse(&l) {
                out.push(r);
            }
        }
    }

    /// Say goodbye and close the connection.
    pub fn quit(mut self) {
        if self.send("QUIT").is_ok() {
            let _ = self.line();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        assert_eq!("/run/upst/upst.sock".parse(), Ok(Addr::Unix("/run/upst/upst.sock".into())));
        assert_eq!("./x.sock".parse(), Ok(Addr::Unix("./x.sock".into())));
        assert_eq!("nas".parse(), Ok(Addr::Tcp(Server { host: "nas".into(), port: DEFAULT_PORT })));
        assert_eq!("[::1]:4000".parse(), Ok(Addr::Tcp(Server { host: "::1".into(), port: 4000 })));
        assert_eq!("nas:4000".parse::<Addr>().unwrap().to_string(), "nas:4000");
        assert_eq!("[::1]:4000".parse::<Addr>().unwrap().to_string(), "[::1]:4000");
        assert!("".parse::<Addr>().is_err());
    }
}
