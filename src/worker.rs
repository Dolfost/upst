//! One background thread per upsd server. It owns the polling
//! connection, reconnects with backoff, and runs commands on their own
//! short-lived connections so credentials never stick to the poller.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::model::{CmdInfo, VarInfo};
use crate::nut::{self, Client, Tracking};
use crate::target::Server;

#[derive(Clone, Debug)]
pub struct Creds {
    pub user: String,
    pub password: String,
}

#[derive(Clone, Debug)]
pub enum Action {
    Command { cmd: String, value: Option<String> },
    Set { var: String, value: String },
}

impl Action {
    pub fn describe(&self) -> String {
        match self {
            Action::Command { cmd, value: Some(v) } => format!("{cmd} {v}"),
            Action::Command { cmd, value: None } => cmd.clone(),
            Action::Set { var, value } => format!("{var} = {value}"),
        }
    }
}

pub enum Request {
    Refresh,
    Interval(Duration),
    Pause(bool),
    Describe { ups: String, var: String },
    Run { ups: String, action: Action, creds: Creds },
}

#[derive(Debug)]
pub enum Update {
    Connecting {
        server: usize,
    },
    Connected {
        server: usize,
        version: Option<String>,
    },
    Down {
        server: usize,
        error: String,
        retry: Duration,
    },
    UpsList {
        server: usize,
        list: Vec<(String, String)>,
    },
    Vars {
        server: usize,
        ups: String,
        vars: Vec<(String, String)>,
    },
    NoData {
        server: usize,
        ups: String,
        error: String,
    },
    Meta {
        server: usize,
        ups: String,
        cmds: Vec<CmdInfo>,
        rw: Vec<String>,
    },
    Info {
        server: usize,
        ups: String,
        var: String,
        info: VarInfo,
    },
    Done {
        server: usize,
        ups: String,
        action: Action,
        result: Result<String, (String, bool)>,
    },
    /// From the recorder, see `archive`.
    Archive(crate::archive::Event),
}

pub struct Config {
    pub index: usize,
    pub server: Server,
    /// `None` polls every UPS the server lists.
    pub names: Option<Vec<String>>,
    pub interval: Duration,
    pub timeout: Duration,
    /// Fetch instant commands and writable variables.
    pub meta: bool,
}

enum End {
    /// The UI went away.
    Quit,
    /// The connection broke; true if it got as far as polling.
    Lost(nut::Error, bool),
}

struct Worker {
    cfg: Config,
    rx: Receiver<Request>,
    tx: Sender<Update>,
    paused: bool,
}

pub fn spawn(cfg: Config, tx: Sender<Update>) -> Sender<Request> {
    let (req_tx, rx) = mpsc::channel();
    let name = format!("upst {}", cfg.server);
    let worker = Worker { cfg, rx, tx, paused: false };
    thread::Builder::new().name(name).spawn(move || worker.run()).expect("spawn worker thread");
    req_tx
}

impl Worker {
    fn send(&self, u: Update) -> bool {
        self.tx.send(u).is_ok()
    }

    fn run(mut self) {
        let min = Duration::from_secs(1);
        let mut backoff = min;
        loop {
            if !self.send(Update::Connecting { server: self.cfg.index }) {
                return;
            }
            match self.session() {
                End::Quit => return,
                End::Lost(err, polled) => {
                    if polled {
                        backoff = min;
                    }
                    let msg = Update::Down { server: self.cfg.index, error: err.to_string(), retry: backoff };
                    if !self.send(msg) || !self.idle(backoff) {
                        return;
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
    }

    /// Wait out a reconnect delay; `false` once the UI is gone. A refresh
    /// request skips the rest of the wait.
    fn idle(&mut self, d: Duration) -> bool {
        let until = Instant::now() + d;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(Request::Refresh) => return true,
                Ok(Request::Interval(i)) => self.cfg.interval = i,
                Ok(Request::Pause(p)) => self.paused = p,
                Ok(Request::Run { ups, action, creds }) => self.run_action(ups, action, creds),
                Ok(Request::Describe { .. }) => {}
                Err(RecvTimeoutError::Timeout) => return true,
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    fn session(&mut self) -> End {
        let idx = self.cfg.index;
        let mut c = match Client::connect(&self.cfg.server.host, self.cfg.server.port, self.cfg.timeout) {
            Ok(c) => c,
            Err(e) => return End::Lost(e, false),
        };
        let version = c.server_version().ok().map(|v| short_version(&v));
        if !self.send(Update::Connected { server: idx, version }) {
            return End::Quit;
        }

        let mut names = match self.discover(&mut c) {
            Ok(n) => n,
            Err(e) if e.is_fatal() => return End::Lost(e, false),
            Err(_) => Vec::new(),
        };
        for ups in &names {
            if let Err(e) = self.meta(&mut c, ups) {
                return End::Lost(e, false);
            }
        }

        let mut last_list = Instant::now();
        loop {
            if !self.paused {
                for ups in &names {
                    let update = match c.list_vars(ups) {
                        Ok(vars) => Update::Vars { server: idx, ups: ups.clone(), vars },
                        Err(e) if e.is_fatal() => return End::Lost(e, true),
                        Err(e) => Update::NoData { server: idx, ups: ups.clone(), error: e.to_string() },
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
                    Ok(n) => {
                        for ups in n.iter().filter(|u| !names.contains(u)) {
                            if let Err(e) = self.meta(&mut c, ups) {
                                return End::Lost(e, true);
                            }
                        }
                        names = n;
                    }
                    Err(e) if e.is_fatal() => return End::Lost(e, true),
                    Err(_) => {}
                }
            }

            let next = Instant::now() + self.cfg.interval;
            loop {
                let left = next.saturating_duration_since(Instant::now());
                match self.rx.recv_timeout(left) {
                    Ok(Request::Refresh) => break,
                    Ok(Request::Interval(i)) => {
                        self.cfg.interval = i;
                        break;
                    }
                    Ok(Request::Pause(p)) => {
                        self.paused = p;
                        if !p {
                            break;
                        }
                    }
                    Ok(Request::Describe { ups, var }) => {
                        let info = match describe(&mut c, &ups, &var) {
                            Ok(i) => i,
                            Err(e) if e.is_fatal() => return End::Lost(e, true),
                            Err(_) => VarInfo::default(),
                        };
                        if !self.send(Update::Info { server: idx, ups, var, info }) {
                            return End::Quit;
                        }
                    }
                    Ok(Request::Run { ups, action, creds }) => self.run_action(ups, action, creds),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        c.logout();
                        return End::Quit;
                    }
                }
            }
        }
    }

    /// UPS names to poll, announcing their descriptions to the UI.
    fn discover(&mut self, c: &mut Client) -> nut::Result<Vec<String>> {
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
        self.send(Update::UpsList { server: self.cfg.index, list });
        Ok(names)
    }

    fn meta(&mut self, c: &mut Client, ups: &str) -> nut::Result<()> {
        if !self.cfg.meta {
            return Ok(());
        }
        let names = soft(c.list_commands(ups), Vec::new())?;
        let mut cmds = Vec::with_capacity(names.len());
        for name in names {
            let desc = soft(c.command_description(ups, &name), None)?;
            cmds.push(CmdInfo { name, desc });
        }
        let rw = soft(c.list_rw(ups), Vec::new())?.into_iter().map(|(k, _)| k).collect();
        self.send(Update::Meta { server: self.cfg.index, ups: ups.to_string(), cmds, rw });
        Ok(())
    }

    fn run_action(&self, ups: String, action: Action, creds: Creds) {
        let tx = self.tx.clone();
        let server = self.cfg.server.clone();
        let (idx, timeout) = (self.cfg.index, self.cfg.timeout);
        thread::spawn(move || {
            let result = execute(&server, timeout, &ups, &action, &creds).map_err(|e| (e.to_string(), e.is_auth()));
            let _ = tx.send(Update::Done { server: idx, ups, action, result });
        });
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

fn describe(c: &mut Client, ups: &str, var: &str) -> nut::Result<VarInfo> {
    let mut info = VarInfo { desc: c.var_description(ups, var)?, ..VarInfo::default() };
    info.kind = c.var_type(ups, var).unwrap_or_default();
    if info.kind.iter().any(|k| k == "ENUM") {
        info.enum_values = c.list_enum(ups, var).unwrap_or_default();
    }
    if info.kind.iter().any(|k| k == "RANGE") {
        info.ranges = c.list_range(ups, var).unwrap_or_default();
    }
    Ok(info)
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
    #[test]
    fn shortens_banner() {
        assert_eq!(super::short_version("Network UPS Tools upsd 2.8.4 - https://networkupstools.org/"), "upsd 2.8.4");
    }
}
