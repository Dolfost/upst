//! A poller: a background thread per upsd server that owns the polling
//! connection, reconnects with backoff, and reports what it gets as
//! [`Update`]s. Instant commands and SET VAR run on short-lived
//! connections of their own, so credentials never stick to the poller.
//!
//! ```no_run
//! use std::time::Duration;
//! use upst_core::poll::{Config, Poller, Update};
//! use upst_core::target::Server;
//!
//! let mut cfg = Config::new(Server::localhost());
//! cfg.interval = Duration::from_secs(5);
//! let (poller, updates) = Poller::channel(cfg)?;
//! for u in updates.iter().take(100) {
//!     match u {
//!         Update::Vars { ups, vars, at } => println!("{ups}: {} variables", vars.len()),
//!         Update::Down { error, retry } => println!("upsd is down ({error}), retrying in {retry:?}"),
//!         _ => {}
//!     }
//! }
//! poller.stop();
//! # Ok::<(), std::io::Error>(())
//! ```

use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::nut::{self, Client, CmdInfo, Tracking, VarInfo};
use crate::target::Server;

/// A user name and password for upsd, for commands and SET VAR.
#[derive(Clone, PartialEq, Eq)]
pub struct Creds {
    /// The user name, as in `upsd.users`.
    pub user: String,
    /// The password.
    pub password: String,
}

/// Shows the user name only.
impl fmt::Debug for Creds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Creds").field("user", &self.user).field("password", &"<hidden>").finish()
    }
}

/// Something to do on a UPS that needs a login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// An instant command, with an optional value.
    Command {
        /// The command, e.g. `beeper.mute`.
        cmd: String,
        /// A value, for the few commands that take one.
        value: Option<String>,
    },
    /// Set a writable variable.
    Set {
        /// The variable.
        var: String,
        /// Its new value.
        value: String,
    },
}

impl Action {
    /// `cmd`, `cmd value` or `var = value`.
    pub fn describe(&self) -> String {
        match self {
            Action::Command { cmd, value: Some(v) } => format!("{cmd} {v}"),
            Action::Command { cmd, value: None } => cmd.clone(),
            Action::Set { var, value } => format!("{var} = {value}"),
        }
    }
}

/// Why an [`Action`] failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What upsd (or the connection) said.
    pub message: String,
    /// True when the credentials or their permissions are the problem.
    pub auth: bool,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

enum Request {
    Refresh,
    Interval(Duration),
    Pause(bool),
    Describe { ups: String, var: String },
    Run { ups: String, action: Action, creds: Creds },
    Stop,
}

/// What a [`Poller`] reports.
#[derive(Debug)]
#[non_exhaustive]
pub enum Update {
    /// Connecting to upsd (again).
    Connecting,
    /// Connected; upsd's version (e.g. `upsd 2.8.4`) when it says.
    Connected {
        /// The short version, e.g. `upsd 2.8.4`.
        version: Option<String>,
    },
    /// The connection failed or broke; the next try is in `retry`.
    Down {
        /// What went wrong.
        error: String,
        /// How long until the next try.
        retry: Duration,
    },
    /// The UPSes the poller watches, with their descriptions.
    UpsList {
        /// (name, description) pairs.
        list: Vec<(String, String)>,
    },
    /// A poll of one UPS: every variable and its value. Feed it to
    /// [`Ups::apply`](crate::Ups::apply).
    Vars {
        /// The UPS name.
        ups: String,
        /// Every variable and its value.
        vars: Vec<(String, String)>,
        /// When the answer came.
        at: SystemTime,
    },
    /// upsd answers, but has no data for this UPS. Feed it to
    /// [`Ups::lost`](crate::Ups::lost) as [`Link::Stale`](crate::Link::Stale).
    NoData {
        /// The UPS name.
        ups: String,
        /// What upsd said.
        error: String,
    },
    /// The instant commands and writable variables of a UPS, when
    /// [`Config::meta`] is on.
    Meta {
        /// The UPS name.
        ups: String,
        /// Its instant commands.
        cmds: Vec<CmdInfo>,
        /// Its writable variables.
        rw: Vec<String>,
    },
    /// What upsd says about a variable, after [`Poller::describe`].
    Info {
        /// The UPS name.
        ups: String,
        /// The variable.
        var: String,
        /// What upsd says about it.
        info: VarInfo,
    },
    /// How an action from [`Poller::run`] went: what upsd answered, or
    /// why it failed.
    Done {
        /// The UPS name.
        ups: String,
        /// The action.
        action: Action,
        /// `done`, `accepted by upsd` or similar, or the failure.
        result: Result<String, Failure>,
    },
}

/// What a [`Poller`] watches and how often.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
    /// The upsd server.
    pub server: Server,
    /// The UPSes to poll; `None` polls every UPS the server lists, and
    /// picks up new ones within a minute.
    pub names: Option<Vec<String>>,
    /// Time between polls; [`Poller::set_interval`] changes it. There is
    /// no lower bound: zero polls back to back.
    pub interval: Duration,
    /// Timeout for connecting and for every answer.
    pub timeout: Duration,
    /// Fetch the instant commands and writable variables of each UPS
    /// ([`Update::Meta`]).
    pub meta: bool,
}

impl Config {
    /// Every UPS on `server`, every 2 seconds, with a 5 second timeout and
    /// no [`Update::Meta`].
    pub fn new(server: Server) -> Config {
        Config { server, names: None, interval: Duration::from_secs(2), timeout: Duration::from_secs(5), meta: false }
    }
}

type Callback = Arc<Mutex<Box<dyn FnMut(Update) -> bool + Send>>>;

/// How often a wait looks at the stop flag, which a callback on an action
/// thread can set.
const STOP_CHECK: Duration = Duration::from_millis(250);

thread_local! {
    /// Set while this thread runs a callback, which must not wait for the
    /// poller to end: the poller may be waiting for the callback.
    static IN_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A running poller for one upsd server. Dropping it stops the thread
/// soon after (once a network call in flight returns); [`Poller::stop`]
/// also waits for that.
#[derive(Debug)]
pub struct Poller {
    tx: Sender<Request>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Poller {
    /// Start polling. `on_update` is called with every update, from the
    /// poller's thread (and from the threads of running actions, one call
    /// at a time); returning `false` stops the poller. It should not block
    /// for long: the poller waits for it, and so does [`Poller::stop`].
    pub fn spawn(cfg: Config, on_update: impl FnMut(Update) -> bool + Send + 'static) -> io::Result<Poller> {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let name = format!("upst {}", cfg.server);
        let worker =
            Worker { cfg, rx, out: Arc::new(Mutex::new(Box::new(on_update))), stop: stop.clone(), paused: false };
        let thread = thread::Builder::new().name(name).spawn(move || worker.run())?;
        Ok(Poller { tx, stop, thread: Some(thread) })
    }

    /// Start polling, with the updates coming through a channel. The
    /// poller stops when the receiver is dropped, at its next update.
    pub fn channel(cfg: Config) -> io::Result<(Poller, Receiver<Update>)> {
        let (tx, rx) = mpsc::channel();
        let poller = Poller::spawn(cfg, move |u| tx.send(u).is_ok())?;
        Ok((poller, rx))
    }

    fn request(&self, r: Request) {
        let _ = self.tx.send(r);
    }

    /// Poll now, or try to connect now while waiting to reconnect.
    pub fn refresh(&self) {
        self.request(Request::Refresh);
    }

    /// Poll every `interval` from now on, starting with a poll now.
    pub fn set_interval(&self, interval: Duration) {
        self.request(Request::Interval(interval));
    }

    /// Stop polling (`true`) but keep the connection, or go on.
    pub fn pause(&self, paused: bool) {
        self.request(Request::Pause(paused));
    }

    /// Ask upsd about a variable; the answer comes as [`Update::Info`].
    /// While upsd is unreachable the question is dropped: ask again after
    /// [`Update::Connected`].
    pub fn describe(&self, ups: &str, var: &str) {
        self.request(Request::Describe { ups: ups.to_string(), var: var.to_string() });
    }

    /// Log in on a connection of its own and run `action`, following its
    /// result when upsd tracks it; the outcome comes as [`Update::Done`].
    pub fn run(&self, ups: &str, action: Action, creds: Creds) {
        self.request(Request::Run { ups: ups.to_string(), action, creds });
    }

    /// Whether the thread still runs.
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Stop the thread and wait for it to end, which takes up to the
    /// timeout when a network call is in flight. An action from
    /// [`Poller::run`] already under way finishes on its own thread, but its
    /// [`Update::Done`] is no longer reported. Called from the callback, it
    /// only tells the thread to stop, like dropping the poller.
    pub fn stop(mut self) {
        self.signal();
        if let Some(t) = self.thread.take()
            && !IN_CALLBACK.get()
        {
            let _ = t.join();
        }
    }

    fn signal(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.request(Request::Stop);
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.signal();
    }
}

enum End {
    /// Stopped, or nobody listens any more.
    Quit,
    /// The connection broke; true if it got as far as polling.
    Lost(nut::Error, bool),
}

struct Worker {
    cfg: Config,
    rx: Receiver<Request>,
    out: Callback,
    stop: Arc<AtomicBool>,
    paused: bool,
}

/// Report an update; false once the poller has to stop.
fn emit(out: &Callback, stop: &AtomicBool, u: Update) -> bool {
    if stop.load(Ordering::SeqCst) {
        return false;
    }
    let mut f = out.lock().unwrap_or_else(|e| e.into_inner());
    IN_CALLBACK.set(true);
    let go_on = f(u);
    IN_CALLBACK.set(false);
    if !go_on {
        stop.store(true, Ordering::SeqCst);
    }
    go_on
}

impl Worker {
    fn send(&self, u: Update) -> bool {
        emit(&self.out, &self.stop, u)
    }

    fn run(mut self) {
        let min = Duration::from_secs(1);
        let mut backoff = min;
        loop {
            if !self.send(Update::Connecting) {
                return;
            }
            match self.session() {
                End::Quit => return,
                End::Lost(err, polled) => {
                    if polled {
                        backoff = min;
                    }
                    let msg = Update::Down { error: err.to_string(), retry: backoff };
                    if !self.send(msg) || !self.idle(backoff) {
                        return;
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
    }

    /// The next request before `until` (any time for `None`), or `None`
    /// once that is past. A stop, or the handle going away, comes as
    /// [`Request::Stop`].
    fn next_request(&self, until: Option<Instant>) -> Option<Request> {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return Some(Request::Stop);
            }
            // No deadline: an interval too long to count waits for requests.
            let left = until.map_or(STOP_CHECK, |u| u.saturating_duration_since(Instant::now()).min(STOP_CHECK));
            match self.rx.recv_timeout(left) {
                Ok(r) => return Some(r),
                Err(RecvTimeoutError::Disconnected) => return Some(Request::Stop),
                Err(RecvTimeoutError::Timeout) if until.is_some_and(|u| Instant::now() >= u) => return None,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Wait out a reconnect delay; `false` once stopped. A refresh skips
    /// the rest of the wait.
    fn idle(&mut self, d: Duration) -> bool {
        let until = Instant::now().checked_add(d);
        loop {
            match self.next_request(until) {
                None | Some(Request::Refresh) => return true,
                Some(Request::Interval(i)) => self.cfg.interval = i,
                Some(Request::Pause(p)) => self.paused = p,
                Some(Request::Run { ups, action, creds }) => self.run_action(ups, action, creds),
                Some(Request::Describe { .. }) => {}
                Some(Request::Stop) => return false,
            }
        }
    }

    fn session(&mut self) -> End {
        let mut c = match Client::connect(&self.cfg.server.host, self.cfg.server.port, self.cfg.timeout) {
            Ok(c) => c,
            Err(e) => return End::Lost(e, false),
        };
        // An upsd that refuses VER is fine; one that does not answer is not.
        let version = match c.server_version() {
            Ok(v) => Some(short_version(&v)),
            Err(e) if e.is_fatal() => return End::Lost(e, false),
            Err(_) => None,
        };
        if !self.send(Update::Connected { version }) {
            return End::Quit;
        }

        let mut names = match self.discover(&mut c) {
            Ok(Some(n)) => n,
            Ok(None) => return End::Quit,
            Err(e) if e.is_fatal() => return End::Lost(e, false),
            Err(_) => Vec::new(),
        };
        for ups in &names {
            match self.meta(&mut c, ups) {
                Ok(true) => {}
                Ok(false) => return End::Quit,
                Err(e) => return End::Lost(e, false),
            }
        }

        let mut last_list = Instant::now();
        loop {
            if !self.paused {
                for ups in &names {
                    let update = match c.list_vars(ups) {
                        Ok(vars) => Update::Vars { ups: ups.clone(), vars, at: SystemTime::now() },
                        Err(e) if e.is_fatal() => return End::Lost(e, true),
                        Err(e) => Update::NoData { ups: ups.clone(), error: e.to_string() },
                    };
                    if !self.send(update) {
                        return End::Quit;
                    }
                }
            }
            // Pick up UPSes added to a server we watch as a whole.
            if self.cfg.names.is_none() && last_list.elapsed() > Duration::from_secs(60) {
                last_list = Instant::now();
                match self.discover(&mut c) {
                    Ok(Some(n)) => {
                        for ups in n.iter().filter(|u| !names.contains(u)) {
                            match self.meta(&mut c, ups) {
                                Ok(true) => {}
                                Ok(false) => return End::Quit,
                                Err(e) => return End::Lost(e, true),
                            }
                        }
                        names = n;
                    }
                    Ok(None) => return End::Quit,
                    Err(e) if e.is_fatal() => return End::Lost(e, true),
                    Err(_) => {}
                }
            }

            let next = Instant::now().checked_add(self.cfg.interval);
            loop {
                match self.next_request(next) {
                    None | Some(Request::Refresh) => break,
                    Some(Request::Interval(i)) => {
                        self.cfg.interval = i;
                        break;
                    }
                    Some(Request::Pause(p)) => {
                        self.paused = p;
                        if !p {
                            break;
                        }
                    }
                    Some(Request::Describe { ups, var }) => {
                        let info = match c.var_info(&ups, &var) {
                            Ok(i) => i,
                            Err(e) if e.is_fatal() => return End::Lost(e, true),
                            Err(_) => VarInfo::default(),
                        };
                        if !self.send(Update::Info { ups, var, info }) {
                            return End::Quit;
                        }
                    }
                    Some(Request::Run { ups, action, creds }) => self.run_action(ups, action, creds),
                    Some(Request::Stop) => {
                        c.logout();
                        return End::Quit;
                    }
                }
            }
        }
    }

    /// UPS names to poll, announcing them with their descriptions;
    /// `None` once the poller has to stop.
    fn discover(&mut self, c: &mut Client) -> nut::Result<Option<Vec<String>>> {
        let listed = c.list_ups();
        let list = match (&self.cfg.names, listed) {
            (None, Ok(l)) => l,
            (None, Err(e)) => return Err(e),
            (Some(names), listed) => {
                if let Err(e) = &listed
                    && e.is_fatal()
                {
                    return Err(listed.unwrap_err());
                }
                let listed = listed.unwrap_or_default();
                names
                    .iter()
                    .map(|n| {
                        let desc = listed.iter().find(|(l, _)| l == n).map(|(_, d)| d.clone());
                        (n.clone(), desc.unwrap_or_default())
                    })
                    .collect()
            }
        };
        let names = list.iter().map(|(n, _)| n.clone()).collect();
        Ok(self.send(Update::UpsList { list }).then_some(names))
    }

    /// Announce the commands and writable variables of a UPS; `false`
    /// once the poller has to stop.
    fn meta(&mut self, c: &mut Client, ups: &str) -> nut::Result<bool> {
        if !self.cfg.meta {
            return Ok(true);
        }
        let names = soft(c.list_commands(ups), Vec::new())?;
        let mut cmds = Vec::with_capacity(names.len());
        for name in names {
            let desc = soft(c.command_description(ups, &name), None)?;
            cmds.push(CmdInfo { name, desc });
        }
        let rw = soft(c.list_rw(ups), Vec::new())?.into_iter().map(|(k, _)| k).collect();
        Ok(self.send(Update::Meta { ups: ups.to_string(), cmds, rw }))
    }

    fn run_action(&self, ups: String, action: Action, creds: Creds) {
        let (out, stop) = (self.out.clone(), self.stop.clone());
        let server = self.cfg.server.clone();
        let timeout = self.cfg.timeout;
        let (ups2, action2) = (ups.clone(), action.clone());
        let spawned = thread::Builder::new().name(format!("upst {server} action")).spawn(move || {
            let result = execute(&server, timeout, &ups, &action, &creds)
                .map_err(|e| Failure { message: e.to_string(), auth: e.is_auth() });
            emit(&out, &stop, Update::Done { ups, action, result });
        });
        if let Err(e) = spawned {
            // Say so rather than leave the caller waiting.
            let result = Err(Failure { message: e.to_string(), auth: false });
            self.send(Update::Done { ups: ups2, action: action2, result });
        }
    }
}

/// Server-side refusals (unsupported, access) fall back to a default;
/// a broken connection is passed on.
fn soft<T>(r: nut::Result<T>, default: T) -> nut::Result<T> {
    match r {
        Ok(v) => Ok(v),
        Err(e) if e.is_fatal() => Err(e),
        Err(_) => Ok(default),
    }
}

/// `Network UPS Tools upsd 2.8.4 - https://...` -> `upsd 2.8.4`.
fn short_version(v: &str) -> String {
    let head = v.split(" - ").next().unwrap_or(v).trim();
    head.strip_prefix("Network UPS Tools ").unwrap_or(head).to_string()
}

/// Log in, run, and follow the result when upsd can track it.
fn execute(server: &Server, timeout: Duration, ups: &str, action: &Action, creds: &Creds) -> nut::Result<String> {
    let mut c = Client::connect(&server.host, server.port, timeout)?;
    c.authenticate(&creds.user, &creds.password)?;
    let tracking = c.enable_tracking();
    let id = match action {
        Action::Command { cmd, value } => c.instcmd(ups, cmd, value.as_deref())?,
        Action::Set { var, value } => c.set_var(ups, var, value)?,
    };
    let result = match id.filter(|_| tracking) {
        None => Ok("accepted by upsd".to_string()),
        Some(id) => {
            let until = Instant::now() + Duration::from_secs(15);
            loop {
                match c.tracking(&id)? {
                    Tracking::Success => break Ok("done".to_string()),
                    Tracking::Failed(e) => break Err(e),
                    Tracking::Pending if Instant::now() > until => {
                        break Ok("accepted, still pending after 15s".to_string());
                    }
                    Tracking::Pending => thread::sleep(Duration::from_millis(250)),
                }
            }
        }
    };
    c.logout();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_banner() {
        assert_eq!(short_version("Network UPS Tools upsd 2.8.4 - https://networkupstools.org/"), "upsd 2.8.4");
    }

    #[test]
    fn hides_the_password() {
        let c = Creds { user: "admin".into(), password: "hunter2".into() };
        let s = format!("{c:?}");
        assert!(s.contains("admin") && !s.contains("hunter2"), "{s}");
    }
}
