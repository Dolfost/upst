//! `upst record`: polls upsd the way the dashboard does, appends every
//! change to a journal per UPS (see `journal`), and serves the journals
//! over a Unix socket or TCP, so upst opens with the history of the last
//! days instead of an empty chart.
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
//! Errors come back as `ERR <what>`.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, bail};

use crate::journal::{self, Journal, Record};
use crate::model::unix;
use crate::target::{Server, parse_server};
use crate::worker::{self, Request, Update};

pub const DEFAULT_PORT: u16 = 3494;

/// Where a recorder listens: a Unix socket path, or `host[:port]`.
#[derive(Clone, Debug, PartialEq)]
pub enum Addr {
    Unix(PathBuf),
    Tcp(Server),
}

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

/// Sockets upst looks for when no recorder is named: the system service,
/// then a user service.
pub fn default_sockets() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/run/upst/upst.sock")];
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        v.push(PathBuf::from(dir).join("upst/upst.sock"));
    }
    v
}

pub enum Stream {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Stream {
    pub fn connect(addr: &Addr, timeout: Duration) -> io::Result<Stream> {
        let s = match addr {
            Addr::Unix(p) => Stream::Unix(UnixStream::connect(p)?),
            Addr::Tcp(srv) => {
                let mut last = None;
                let mut found = None;
                for a in (srv.host.as_str(), srv.port).to_socket_addrs()? {
                    match TcpStream::connect_timeout(&a, timeout) {
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
        s.timeouts(Some(timeout))?;
        Ok(s)
    }

    fn timeouts(&self, t: Option<Duration>) -> io::Result<()> {
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

    fn try_clone(&self) -> io::Result<Stream> {
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
pub struct Listed {
    pub label: String,
    pub first: f64,
    pub last: f64,
}

// ---- client ---------------------------------------------------------------

pub struct Client {
    r: BufReader<Stream>,
    w: Stream,
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

impl Client {
    pub fn connect(addr: &Addr, timeout: Duration) -> io::Result<Client> {
        let w = Stream::connect(addr, timeout)?;
        Ok(Client { r: BufReader::new(w.try_clone()?), w })
    }

    fn send(&mut self, line: &str) -> io::Result<()> {
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

    pub fn version(&mut self) -> io::Result<String> {
        self.send("VER")?;
        self.line()
    }

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
            if let ["UPS", label, first, last] = w.as_slice() {
                let num = |s: &str| s.parse::<f64>().map_err(|_| bad(format!("bad time in {l:?}")));
                out.push(Listed { label: label.to_string(), first: num(first)?, last: num(last)? });
            }
        }
    }

    /// The journal from `since` on, opening with the state just before it.
    pub fn get(&mut self, label: &str, since: f64) -> io::Result<Vec<Record>> {
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
}

// ---- server ---------------------------------------------------------------

#[derive(Clone, Debug)]
struct Entry {
    path: PathBuf,
    first: f64,
    last: f64,
}

type Index = Arc<Mutex<BTreeMap<String, Entry>>>;

fn serve(stream: Stream, index: Index) -> io::Result<()> {
    // Idle clients are dropped after a while; upst reconnects on demand.
    stream.timeouts(Some(Duration::from_secs(300)))?;
    let mut out = BufWriter::new(stream.try_clone()?);
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            [] => continue,
            ["VER"] => writeln!(out, "upst {} journal {}", env!("CARGO_PKG_VERSION"), journal::VERSION)?,
            ["LIST"] => {
                let list: Vec<(String, Entry)> =
                    index.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                writeln!(out, "BEGIN LIST")?;
                for (label, e) in list {
                    writeln!(out, "UPS {label} {} {}", e.first.floor() as i64, e.last.floor() as i64)?;
                }
                writeln!(out, "END LIST")?;
            }
            ["GET", label, rest @ ..] if rest.len() <= 1 => {
                let since = match rest.first().map(|s| s.parse::<f64>()) {
                    None => f64::NEG_INFINITY,
                    Some(Ok(t)) => t,
                    Some(Err(_)) => {
                        writeln!(out, "ERR INVALID-ARGUMENT")?;
                        out.flush()?;
                        continue;
                    }
                };
                let entry = index.lock().unwrap().get(*label).cloned();
                match entry.map(|e| journal::read(&e.path)) {
                    None => writeln!(out, "ERR UNKNOWN-UPS")?,
                    Some(Err(e)) => writeln!(out, "ERR READ-FAILED {e}")?,
                    Some(Ok(records)) => {
                        writeln!(out, "BEGIN {label}")?;
                        for r in journal::since(records, since) {
                            writeln!(out, "{r}")?;
                        }
                        writeln!(out, "END")?;
                    }
                }
            }
            ["QUIT"] | ["LOGOUT"] => {
                writeln!(out, "OK Goodbye")?;
                return out.flush();
            }
            _ => writeln!(out, "ERR UNKNOWN-COMMAND")?,
        }
        out.flush()?;
    }
}

fn bind_unix(path: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    if fs::symlink_metadata(path).is_ok() {
        if UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, "another recorder is listening there"));
        }
        fs::remove_file(path)?;
    }
    let l = UnixListener::bind(path)?;
    // History is no more secret than what upsd tells anyone who asks.
    fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
    Ok(l)
}

fn accept_loop<S: Send + 'static>(next: impl Fn() -> io::Result<S>, wrap: fn(S) -> Stream, index: Index) {
    loop {
        match next() {
            Ok(s) => {
                let index = index.clone();
                thread::spawn(move || {
                    let _ = serve(wrap(s), index);
                });
            }
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
    }
}

// ---- recording ------------------------------------------------------------

pub struct Config {
    pub groups: Vec<(Server, Option<Vec<String>>)>,
    pub interval: Duration,
    pub timeout: Duration,
    pub keep: Duration,
    pub dir: PathBuf,
    pub listen: Vec<Addr>,
    /// No log lines (for the in-process demo recorder).
    pub quiet: bool,
}

pub struct Recorder {
    cfg: Config,
    journals: BTreeMap<String, Journal>,
    index: Index,
    /// Where each listener ended up (TCP port 0 gets a real port).
    pub bound: Vec<Addr>,
    /// Server index per open journal.
    server_of: BTreeMap<String, usize>,
    /// Last write error per journal, logged once.
    failing: BTreeMap<String, String>,
    down: Vec<Option<String>>,
}

impl Recorder {
    /// Open the journal directory and start listening. Polling starts
    /// with `run`.
    pub fn new(cfg: Config) -> anyhow::Result<Recorder> {
        fs::create_dir_all(&cfg.dir).with_context(|| format!("creating {}", cfg.dir.display()))?;
        let index: Index = Arc::default();
        let mut rec = Recorder {
            journals: BTreeMap::new(),
            index: index.clone(),
            bound: Vec::new(),
            server_of: BTreeMap::new(),
            failing: BTreeMap::new(),
            down: vec![None; cfg.groups.len()],
            cfg,
        };
        rec.scan()?;
        for addr in rec.cfg.listen.clone() {
            let index = index.clone();
            match &addr {
                Addr::Unix(path) => {
                    let l = bind_unix(path).with_context(|| format!("listening on {}", path.display()))?;
                    thread::spawn(move || accept_loop(move || l.accept().map(|(s, _)| s), Stream::Unix, index));
                    rec.bound.push(addr.clone());
                }
                Addr::Tcp(srv) => {
                    let l = TcpListener::bind((srv.host.as_str(), srv.port))
                        .with_context(|| format!("listening on {addr}"))?;
                    let port = l.local_addr()?.port();
                    thread::spawn(move || accept_loop(move || l.accept().map(|(s, _)| s), Stream::Tcp, index));
                    rec.bound.push(Addr::Tcp(Server { host: srv.host.clone(), port }));
                }
            }
        }
        rec.log(format!(
            "journals in {}, kept for {}, listening on {}",
            rec.cfg.dir.display(),
            crate::fmt::span(rec.cfg.keep.as_secs_f64()),
            rec.bound.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", ")
        ));
        Ok(rec)
    }

    fn log(&self, msg: impl AsRef<str>) {
        if !self.cfg.quiet {
            eprintln!("{}", msg.as_ref());
        }
    }

    fn cutoff(&self, now: f64) -> f64 {
        now - self.cfg.keep.as_secs_f64()
    }

    /// Index the journals already on disk; trim old ones, delete expired.
    fn scan(&mut self) -> anyhow::Result<()> {
        let now = unix(SystemTime::now());
        let cutoff = self.cutoff(now);
        let entries = fs::read_dir(&self.cfg.dir).with_context(|| format!("reading {}", self.cfg.dir.display()))?;
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "log") {
                continue;
            }
            let Ok(Some(label)) = journal::read_label(&path) else { continue };
            let mut state = journal::State::default();
            if journal::for_each(&path, |r| state.apply(&r)).is_err() {
                continue;
            }
            let (Some(first), Some(last)) = (state.first, state.last) else { continue };
            if last < cutoff {
                let age = crate::fmt::span(now - last);
                self.log(format!("{label}: nothing newer than {age}, removing {}", path.display()));
                let _ = fs::remove_file(&path);
                continue;
            }
            let first = if first < cutoff - 3600.0 {
                match journal::rewrite(&path, &label, cutoff) {
                    Ok(s) => s.first.unwrap_or(first),
                    Err(err) => {
                        self.log(format!("{label}: trimming failed: {err}"));
                        first
                    }
                }
            } else {
                first
            };
            self.index.lock().unwrap().insert(label, Entry { path, first, last });
        }
        Ok(())
    }

    fn label(&self, server: usize, ups: &str) -> String {
        format!("{ups}@{}", self.cfg.groups[server].0)
    }

    fn journal(&mut self, server: usize, label: &str, t: f64) -> Option<&mut Journal> {
        if !self.journals.contains_key(label) {
            match Journal::open(&self.cfg.dir, label, t) {
                Ok(j) => {
                    self.journals.insert(label.to_string(), j);
                    self.server_of.insert(label.to_string(), server);
                }
                Err(e) => {
                    self.failed(label, &e);
                    return None;
                }
            }
        }
        self.journals.get_mut(label)
    }

    fn failed(&mut self, label: &str, e: &io::Error) {
        let msg = e.to_string();
        if self.failing.get(label) != Some(&msg) {
            self.log(format!("{label}: writing the journal failed: {msg}"));
            self.failing.insert(label.to_string(), msg);
        }
    }

    /// Update the index after a write, and note a failure.
    fn wrote(&mut self, label: &str, r: io::Result<()>) {
        match r {
            Ok(()) => {
                self.failing.remove(label);
            }
            Err(e) => self.failed(label, &e),
        }
        if let Some(j) = self.journals.get(label)
            && let (Some(first), Some(last)) = (j.state.first, j.state.last)
        {
            let e = Entry { path: j.path.clone(), first, last };
            self.index.lock().unwrap().insert(label.to_string(), e);
        }
    }

    /// Write earlier polls into a journal before recording starts, for the
    /// demo.
    pub fn seed(&mut self, server: usize, ups: &str, frames: Vec<(f64, Vec<(String, String)>)>) {
        let label = self.label(server, ups);
        let Some(&(t0, _)) = frames.first() else { return };
        for (t, vars) in frames {
            let Some(j) = self.journal(server, &label, t0) else { return };
            let r = j.poll(t, &vars);
            self.wrote(&label, r);
        }
    }

    /// Poll and record until the process is stopped.
    pub fn run(mut self) -> anyhow::Result<()> {
        let (tx, rx) = mpsc::channel();
        let mut workers: Vec<Sender<Request>> = Vec::new();
        let now = unix(SystemTime::now());
        for (index, (server, names)) in self.cfg.groups.clone().into_iter().enumerate() {
            // Known UPSes get their journals now, so an unreachable upsd
            // is on record too.
            let known: Vec<String> = match &names {
                Some(n) => n.iter().map(|n| format!("{n}@{server}")).collect(),
                None => {
                    let suffix = format!("@{server}");
                    self.index.lock().unwrap().keys().filter(|l| l.ends_with(&suffix)).cloned().collect()
                }
            };
            for label in known {
                if self.journal(index, &label, now).is_some() {
                    self.wrote(&label, Ok(()));
                }
            }
            let cfg = worker::Config {
                index,
                server,
                names,
                interval: self.cfg.interval,
                timeout: self.cfg.timeout,
                meta: false,
            };
            workers.push(worker::spawn(cfg, tx.clone()));
        }
        drop(tx);
        // The workers stop when their request channel closes; keep it.
        let _workers = workers;
        self.record(rx)
    }

    fn record(&mut self, rx: Receiver<Update>) -> anyhow::Result<()> {
        let mut compacted = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(u) => self.update(u),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => bail!("the pollers stopped"),
            }
            let now = unix(SystemTime::now());
            let labels: Vec<String> = self.journals.keys().cloned().collect();
            for label in &labels {
                let r = self.journals.get_mut(label).map_or(Ok(()), |j| j.tick(now));
                if r.is_err() {
                    self.wrote(label, r);
                }
            }
            if compacted.elapsed() > Duration::from_secs(3600) {
                compacted = Instant::now();
                let cutoff = self.cutoff(now);
                for label in &labels {
                    let r = self.journals.get_mut(label).map_or(Ok(false), |j| j.compact(cutoff));
                    self.wrote(label, r.map(|_| ()));
                }
            }
        }
    }

    fn update(&mut self, u: Update) {
        let now = unix(SystemTime::now());
        match u {
            Update::Connected { server, version } => {
                if self.down[server].take().is_some() {
                    self.log(format!("{}: reachable again", self.cfg.groups[server].0));
                } else {
                    let v = version.unwrap_or_else(|| "upsd".into());
                    self.log(format!("{}: connected to {v}", self.cfg.groups[server].0));
                }
            }
            Update::Down { server, error, .. } => {
                if self.down[server].as_deref() != Some(&error) {
                    self.log(format!("{}: {error}", self.cfg.groups[server].0));
                    self.down[server] = Some(error.clone());
                }
                let labels: Vec<String> =
                    self.server_of.iter().filter(|(_, s)| **s == server).map(|(l, _)| l.clone()).collect();
                for label in labels {
                    let r = self.journals.get_mut(&label).map_or(Ok(()), |j| j.down(now, &error));
                    self.wrote(&label, r);
                }
            }
            Update::UpsList { server, list } => {
                for (name, _) in list {
                    let label = self.label(server, &name);
                    if self.journal(server, &label, now).is_some() {
                        self.wrote(&label, Ok(()));
                    }
                }
            }
            Update::Vars { server, ups, vars } => {
                let label = self.label(server, &ups);
                if let Some(j) = self.journal(server, &label, now) {
                    let r = j.poll(now, &vars);
                    self.wrote(&label, r);
                }
            }
            Update::NoData { server, ups, error } => {
                let label = self.label(server, &ups);
                if let Some(j) = self.journal(server, &label, now) {
                    let fresh = !matches!(j.state.feed, journal::Feed::NoData(_));
                    let r = j.no_data(now, &error);
                    self.wrote(&label, r);
                    if fresh {
                        self.log(format!("{label}: {error}"));
                    }
                }
            }
            Update::Connecting { .. }
            | Update::Meta { .. }
            | Update::Info { .. }
            | Update::Done { .. }
            | Update::Archive(_) => {}
        }
    }
}

/// `$STATE_DIRECTORY` (systemd), else `$XDG_STATE_HOME/upst`, else
/// `~/.local/state/upst`.
pub fn default_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if let Some(d) = env("STATE_DIRECTORY") {
        return std::env::split_paths(&d).next();
    }
    if let Some(d) = env("XDG_STATE_HOME") {
        return Some(PathBuf::from(d).join("upst"));
    }
    env("HOME").map(|h| PathBuf::from(h).join(".local/state/upst"))
}

/// `$RUNTIME_DIRECTORY/upst.sock` (systemd), else
/// `$XDG_RUNTIME_DIR/upst/upst.sock`.
pub fn default_listen() -> Option<Addr> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if let Some(d) = env("RUNTIME_DIRECTORY") {
        return std::env::split_paths(&d).next().map(|d| Addr::Unix(d.join("upst.sock")));
    }
    env("XDG_RUNTIME_DIR").map(|d| Addr::Unix(PathBuf::from(d).join("upst/upst.sock")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Kind;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("upst-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn parses_addresses() {
        assert_eq!("/run/upst/upst.sock".parse(), Ok(Addr::Unix("/run/upst/upst.sock".into())));
        assert_eq!("./x.sock".parse(), Ok(Addr::Unix("./x.sock".into())));
        assert_eq!("nas".parse(), Ok(Addr::Tcp(Server { host: "nas".into(), port: DEFAULT_PORT })));
        assert_eq!("[::1]:4000".parse(), Ok(Addr::Tcp(Server { host: "::1".into(), port: 4000 })));
        assert_eq!("nas:4000".parse::<Addr>().unwrap().to_string(), "nas:4000");
        assert_eq!("[::1]:4000".parse::<Addr>().unwrap().to_string(), "[::1]:4000");
    }

    #[test]
    fn records_the_demo_and_serves_it() {
        let dir = temp("rec");
        let server = crate::demo::start().unwrap();
        let sock = dir.join("upst.sock");
        let cfg = Config {
            groups: vec![(server.clone(), Some(vec!["pack".into()]))],
            interval: Duration::from_millis(200),
            timeout: Duration::from_secs(2),
            keep: Duration::from_secs(3600),
            dir: dir.clone(),
            listen: vec![Addr::Unix(sock.clone()), "127.0.0.1:0".parse().unwrap()],
            quiet: true,
        };
        let mut rec = Recorder::new(cfg).unwrap();
        let tcp = rec.bound[1].clone();
        let t0 = (unix(SystemTime::now()) - 100.0).floor();
        let frame = |c: &str| vec![("battery.charge".to_string(), c.to_string()), ("ups.status".into(), "OB".into())];
        rec.seed(0, "pack", vec![(t0, frame("90")), (t0 + 50.0, frame("80"))]);
        thread::spawn(move || rec.run());
        thread::sleep(Duration::from_millis(800));

        let label = format!("pack@{server}");
        let set = |t: f64, k: &str, v: &str| Record::new(t, Kind::Set(k.into(), v.into()));
        for addr in [Addr::Unix(sock), tcp] {
            let mut c = Client::connect(&addr, Duration::from_secs(2)).unwrap();
            assert!(c.version().unwrap().starts_with("upst "));
            let list = c.list().unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].label, label);
            assert_eq!(list[0].first, t0);
            let all = c.get(&label, 0.0).unwrap();
            assert_eq!(all[0].kind, Kind::Start);
            assert!(all.contains(&set(t0 + 50.0, "battery.charge", "80")));
            // Live polls follow the seed.
            assert!(all.iter().any(|r| matches!(&r.kind, Kind::Set(k, _) if k == "device.model")));
            // Asking from the middle starts with the state at that point.
            let tail = c.get(&label, t0 + 10.0).unwrap();
            assert_eq!(tail[0], Record::new(t0, Kind::Start));
            assert!(tail.contains(&set(t0, "battery.charge", "90")));
            assert!(c.get("nope@x", 0.0).is_err());
        }
        // A second recorder on the same socket is refused.
        let again = Config {
            groups: Vec::new(),
            interval: Duration::from_secs(1),
            timeout: Duration::from_secs(1),
            keep: Duration::from_secs(3600),
            dir: dir.clone(),
            listen: vec![Addr::Unix(dir.join("upst.sock"))],
            quiet: true,
        };
        assert!(Recorder::new(again).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
