//! Application state and input handling. Drawing lives in `ui`.

use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Local};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::TableState;

use crate::archive::{self, Fetch};
use crate::cli::{ChargeStyle, ViewArg};
use crate::history::History;
use crate::model::{Link, Ups, dangerous, from_unix, unix};
use crate::recorder::{Addr, Listed};
use crate::status::Severity;
use crate::target::{Server, Target};
use crate::ui::Theme;
use crate::worker::{Action, Creds, Request, Update};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Overview,
    History,
    Variables,
    Events,
}

impl View {
    pub const ALL: [View; 4] = [View::Overview, View::History, View::Variables, View::Events];

    pub fn title(self) -> &'static str {
        match self {
            View::Overview => "Overview",
            View::History => "History",
            View::Variables => "Variables",
            View::Events => "Events",
        }
    }

    /// For narrow headers.
    pub fn short(self) -> &'static str {
        match self {
            View::Overview => "Over",
            View::History => "Hist",
            View::Variables => "Vars",
            View::Events => "Evts",
        }
    }

    fn index(self) -> usize {
        View::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }
}

impl From<ViewArg> for View {
    fn from(v: ViewArg) -> Self {
        match v {
            ViewArg::Overview => View::Overview,
            ViewArg::History => View::History,
            ViewArg::Variables => View::Variables,
            ViewArg::Events => View::Events,
        }
    }
}

/// Chart windows the History view steps through.
pub const WINDOWS: [f64; 11] =
    [60.0, 300.0, 900.0, 1800.0, 3600.0, 10800.0, 21600.0, 43200.0, 86400.0, 172800.0, 604800.0];
/// Poll intervals `<` and `>` step through.
const INTERVALS: [f64; 10] = [0.5, 1.0, 2.0, 3.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0];

#[derive(Clone, Debug, PartialEq)]
pub enum ServerLink {
    Connecting,
    Up,
    Down { error: String, retry_at: Instant },
}

pub struct ServerState {
    pub server: Server,
    pub tx: Sender<Request>,
    pub link: ServerLink,
    pub version: Option<String>,
    pub creds: Option<Creds>,
    connected_once: bool,
    reported_down: bool,
}

#[derive(Clone, Debug)]
pub struct Logged {
    pub at: DateTime<Local>,
    pub ups: String,
    pub severity: Severity,
    pub text: String,
}

/// A single-line text field.
#[derive(Clone, Debug, Default)]
pub struct Input {
    pub text: String,
    /// Cursor position in chars.
    pub cursor: usize,
}

impl Input {
    pub fn new(text: &str) -> Self {
        Input { text: text.to_string(), cursor: text.chars().count() }
    }

    fn byte(&self, char_idx: usize) -> usize {
        self.text.char_indices().nth(char_idx).map_or(self.text.len(), |(i, _)| i)
    }

    /// Returns true if the key was a text-editing key.
    pub fn key(&mut self, k: &KeyEvent) -> bool {
        let len = self.text.chars().count();
        match k.code {
            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char('w') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                let chars: Vec<char> = self.text.chars().collect();
                let mut i = self.cursor;
                while i > 0 && chars[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && chars[i - 1] != ' ' {
                    i -= 1;
                }
                let (a, b) = (self.byte(i), self.byte(self.cursor));
                self.text.replace_range(a..b, "");
                self.cursor = i;
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                let at = self.byte(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let at = self.byte(self.cursor - 1);
                self.text.remove(at);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < len => {
                let at = self.byte(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        true
    }
}

pub enum Dialog {
    Help,
    Commands { selected: usize },
    Confirm { ups: usize, action: Action, desc: Option<String>, typed: Input, error: Option<String> },
    Login { server: usize, user: Input, password: Input, focus: usize, then: Option<(usize, Action)> },
    Edit { ups: usize, var: String, input: Input, selected: usize, error: Option<String> },
}

/// Clickable regions recorded while drawing.
#[derive(Clone, Copy, Debug)]
pub enum Hit {
    Tab(View),
    Ups(usize),
    VarRow(usize),
    Chart(usize),
    DialogRow(usize),
}

pub struct Toast {
    pub text: String,
    pub severity: Severity,
    pub at: Instant,
}

pub struct VarsView {
    pub table: TableState,
    pub filter: Input,
    pub filtering: bool,
}

pub struct HistView {
    pub window: usize,
    pub selected: usize,
    pub zoomed: bool,
    /// Charts the last frame showed, for keyboard and mouse.
    pub count: usize,
}

pub struct EventsView {
    pub offset: usize,
    pub all: bool,
}

pub struct Options {
    pub interval: Duration,
    pub history: Duration,
    pub capacity: Option<f64>,
    /// Capacities of single UPSes, which win over `capacity`.
    pub capacities: Vec<(Target, f64)>,
    pub bell: bool,
    pub view: View,
    pub theme: Theme,
    pub charge_style: ChargeStyle,
    pub user: Option<String>,
    pub password: Option<String>,
}

/// The `upst record` that history comes from.
pub enum RecorderState {
    Off,
    Connecting,
    Ready { addr: Addr, list: Vec<Listed> },
}

pub struct App {
    pub ups: Vec<Ups>,
    pub servers: Vec<ServerState>,
    pub sel: usize,
    pub view: View,
    pub log: VecDeque<Logged>,
    pub interval: Duration,
    pub paused: bool,
    pub theme: Theme,
    pub charge_style: ChargeStyle,
    pub recorder: RecorderState,
    recorder_tx: Option<Sender<Fetch>>,
    pub vars: VarsView,
    pub hist: HistView,
    pub events: EventsView,
    pub dialog: Option<Dialog>,
    pub toast: Option<Toast>,
    pub hits: Vec<(Rect, Hit)>,
    pub quit: bool,
    /// Terminal side effects for the main loop to perform.
    pub bell_pending: bool,
    pub clipboard: Option<String>,
    history_keep: Duration,
    capacity: Option<f64>,
    capacities: Vec<(Target, f64)>,
    bell: bool,
    default_user: Option<String>,
    /// Credentials from the command line, given to every server.
    pending_creds: Option<Creds>,
}

const LOG_MAX: usize = 2000;

impl App {
    pub fn new(opts: Options) -> Self {
        App {
            ups: Vec::new(),
            servers: Vec::new(),
            sel: 0,
            view: opts.view,
            log: VecDeque::new(),
            interval: opts.interval,
            paused: false,
            theme: opts.theme,
            charge_style: opts.charge_style,
            recorder: RecorderState::Off,
            recorder_tx: None,
            vars: VarsView {
                table: TableState::default().with_selected(0),
                filter: Input::default(),
                filtering: false,
            },
            hist: HistView { window: 2, selected: 0, zoomed: false, count: 0 },
            events: EventsView { offset: 0, all: true },
            dialog: None,
            toast: None,
            hits: Vec::new(),
            quit: false,
            bell_pending: false,
            clipboard: None,
            history_keep: opts.history,
            capacity: opts.capacity,
            capacities: opts.capacities,
            bell: opts.bell,
            pending_creds: match (&opts.user, opts.password) {
                (Some(user), Some(password)) => Some(Creds { user: user.clone(), password }),
                _ => None,
            },
            default_user: opts.user,
        }
    }

    pub fn add_server(&mut self, server: Server, tx: Sender<Request>, names: Option<&[String]>) -> usize {
        let idx = self.servers.len();
        self.servers.push(ServerState {
            server: server.clone(),
            tx,
            link: ServerLink::Connecting,
            version: None,
            creds: self.pending_creds.clone(),
            connected_once: false,
            reported_down: false,
        });
        for name in names.unwrap_or_default() {
            self.ensure_ups(idx, name, "");
        }
        idx
    }

    /// History comes from this recorder; polls wait for it.
    pub fn set_recorder(&mut self, tx: Sender<Fetch>) {
        self.recorder = RecorderState::Connecting;
        self.recorder_tx = Some(tx);
    }

    fn new_history(&self) -> History {
        History::new(self.history_keep.as_secs_f64(), self.interval.as_secs_f64())
    }

    /// Find or add a UPS, keeping server order and the selection.
    fn ensure_ups(&mut self, server: usize, name: &str, desc: &str) -> usize {
        if let Some(i) = self.ups.iter().position(|u| u.server == server && u.name == name) {
            if !desc.is_empty() {
                self.ups[i].desc = desc.to_string();
            }
            return i;
        }
        let srv = &self.servers[server].server;
        let capacity = self
            .capacities
            .iter()
            .find(|(t, _)| t.server == *srv && t.ups.as_deref() == Some(name))
            .map_or(self.capacity, |(_, c)| Some(*c));
        let ups = Ups::new(server, &srv.to_string(), name, desc, self.new_history(), capacity);
        let at = self.ups.iter().rposition(|u| u.server <= server).map_or(0, |i| i + 1);
        let selected = self.ups.get(self.sel).map(|u| (u.server, u.name.clone()));
        self.ups.insert(at, ups);
        if let Some((s, n)) = selected {
            self.sel = self.ups.iter().position(|u| u.server == s && u.name == n).unwrap_or(0);
        }
        match self.recorder {
            RecorderState::Off => {}
            RecorderState::Connecting => self.ups[at].held = Some(Vec::new()),
            RecorderState::Ready { .. } => self.fetch_history(at),
        }
        at
    }

    fn find_ups(&self, server: usize, name: &str) -> Option<usize> {
        self.ups.iter().position(|u| u.server == server && u.name == name)
    }

    /// Ask the recorder for a UPS's journal, holding its polls meanwhile.
    fn fetch_history(&mut self, i: usize) {
        let (RecorderState::Ready { addr, list }, Some(tx)) = (&self.recorder, &self.recorder_tx) else {
            return self.release(i);
        };
        let u = &self.ups[i];
        let Some(label) = archive::matching(list, &u.name, &self.servers[u.server].server, addr) else {
            return self.release(i);
        };
        let since = unix(SystemTime::now()) - self.history_keep.as_secs_f64();
        let fetch = Fetch { server: u.server, ups: u.name.clone(), label: label.to_string(), since };
        if tx.send(fetch).is_ok() {
            self.ups[i].held.get_or_insert_with(Vec::new);
        } else {
            self.release(i);
        }
    }

    /// Apply the polls that waited for recorded history.
    fn release(&mut self, i: usize) {
        for (vars, at) in self.ups[i].held.take().unwrap_or_default() {
            self.apply_vars(i, vars, at);
        }
    }

    fn apply_vars(&mut self, i: usize, vars: Vec<(String, String)>, at: SystemTime) {
        let events = self.ups[i].apply(vars, at);
        let label = self.ups[i].label.clone();
        for (sev, text) in events {
            self.log_event(&label, sev, text);
        }
    }

    fn on_archive(&mut self, ev: archive::Event) {
        match ev {
            archive::Event::Ready { addr, list } => {
                self.recorder = RecorderState::Ready { addr, list };
                for i in 0..self.ups.len() {
                    if self.ups[i].held.is_some() {
                        self.fetch_history(i);
                    }
                }
            }
            archive::Event::Off { error } => {
                self.recorder = RecorderState::Off;
                if let Some(e) = error {
                    self.log_event("recorder", Severity::Warn, format!("No recorded history: {e}"));
                }
                for i in 0..self.ups.len() {
                    self.release(i);
                }
            }
            archive::Event::History { server, ups, label, result } => {
                let Some(i) = self.find_ups(server, &ups) else { return };
                match result {
                    Ok(records) if !records.is_empty() => {
                        let until = self.ups[i]
                            .held
                            .as_ref()
                            .and_then(|h| h.first())
                            .map_or_else(|| unix(SystemTime::now()), |(_, at)| unix(*at));
                        let events = archive::replay(&mut self.ups[i], &records, until);
                        let ups_label = self.ups[i].label.clone();
                        for (t, sev, text) in events {
                            self.log_event_at(DateTime::from(from_unix(t)), &ups_label, sev, text, false);
                        }
                        let first = from_unix(records[0].t);
                        let via = match &self.recorder {
                            RecorderState::Ready { addr, .. } => addr.to_string(),
                            _ => String::new(),
                        };
                        self.ups[i].recorded = Some((via, first));
                        let back = crate::fmt::when(first);
                        self.log_event(&ups_label, Severity::Info, format!("Recorded history loaded, back to {back}"));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let ups_label = self.ups[i].label.clone();
                        self.log_event(&ups_label, Severity::Warn, format!("Recorded history of {label}: {e}"));
                    }
                }
                self.release(i);
                // The replay ends with the recorder's view; the live link
                // may already know better.
                if let ServerLink::Down { error, .. } = &self.servers[server].link {
                    let link = Link::Down(error.clone());
                    self.ups[i].lost(link);
                }
            }
        }
    }

    pub fn current(&self) -> Option<&Ups> {
        self.ups.get(self.sel)
    }

    pub fn log_event(&mut self, ups: &str, severity: Severity, text: String) {
        self.log_event_at(Local::now(), ups, severity, text, true);
    }

    /// Log an event that happened at `at`, in time order.
    pub fn log_event_at(&mut self, at: DateTime<Local>, ups: &str, severity: Severity, text: String, ring: bool) {
        if ring && self.bell && severity >= Severity::Warn {
            self.bell_pending = true;
        }
        let pos = self.log.partition_point(|l| l.at <= at);
        let newest = pos == self.log.len();
        self.log.insert(pos, Logged { at, ups: ups.to_string(), severity, text });
        while self.log.len() > LOG_MAX {
            self.log.pop_front();
        }
        if newest && self.events.offset > 0 {
            // Keep the reader's place while new lines arrive on top.
            self.events.offset += 1;
        }
    }

    pub fn toast(&mut self, severity: Severity, text: impl Into<String>) {
        self.toast = Some(Toast { text: text.into(), severity, at: Instant::now() });
    }

    pub fn update(&mut self, u: Update) {
        match u {
            Update::Connecting { server } => {
                if self.servers[server].link != ServerLink::Up {
                    self.servers[server].link = ServerLink::Connecting;
                }
            }
            Update::Connected { server, version } => {
                let s = &mut self.servers[server];
                s.link = ServerLink::Up;
                let label = s.server.to_string();
                let text = match (&version, s.connected_once) {
                    (Some(v), false) => format!("Connected to {v} at {label}"),
                    (None, false) => format!("Connected to upsd at {label}"),
                    (_, true) => format!("Reconnected to {label}"),
                };
                s.version = version;
                s.connected_once = true;
                s.reported_down = false;
                self.log_event(&label, Severity::Info, text);
                // Requests the old connection dropped are asked again.
                for u in self.ups.iter_mut().filter(|u| u.server == server) {
                    u.info_pending.clear();
                }
            }
            Update::Down { server, error, retry } => {
                let s = &mut self.servers[server];
                s.link = ServerLink::Down { error: error.clone(), retry_at: Instant::now() + retry };
                let label = s.server.to_string();
                let first = !s.reported_down;
                s.reported_down = true;
                let mut any = false;
                let mut evs = Vec::new();
                for u in self.ups.iter_mut().filter(|u| u.server == server) {
                    any = true;
                    if let Some((sev, text)) = u.lost(Link::Down(error.clone())) {
                        evs.push((u.label.clone(), sev, text));
                    }
                }
                if !any && first {
                    evs.push((label, Severity::Crit, format!("Cannot reach upsd: {error}")));
                }
                for (l, sev, text) in evs {
                    self.log_event(&l, sev, text);
                }
            }
            Update::UpsList { server, list } => {
                for (name, desc) in list {
                    self.ensure_ups(server, &name, &desc);
                }
            }
            Update::Vars { server, ups, vars } => {
                let i = self.ensure_ups(server, &ups, "");
                let now = SystemTime::now();
                match &mut self.ups[i].held {
                    Some(held) => held.push((vars, now)),
                    None => self.apply_vars(i, vars, now),
                }
            }
            Update::Archive(ev) => self.on_archive(ev),
            Update::NoData { server, ups, error } => {
                let i = self.ensure_ups(server, &ups, "");
                if let Some((sev, text)) = self.ups[i].lost(Link::Stale(error)) {
                    let label = self.ups[i].label.clone();
                    self.log_event(&label, sev, text);
                }
            }
            Update::Meta { server, ups, cmds, rw } => {
                let i = self.ensure_ups(server, &ups, "");
                self.ups[i].cmds = cmds;
                self.ups[i].rw = rw.into_iter().collect();
            }
            Update::Info { server, ups, var, info } => {
                let i = self.ensure_ups(server, &ups, "");
                self.ups[i].info_pending.remove(&var);
                // An open SET dialog was waiting for these choices.
                if let Some(Dialog::Edit { ups, var: v, selected, .. }) = &mut self.dialog
                    && *ups == i
                    && *v == var
                {
                    let current = self.ups[i].get(&var).unwrap_or("");
                    *selected = info.enum_values.iter().position(|e| e == current).unwrap_or(0);
                }
                self.ups[i].info.insert(var, info);
            }
            Update::Done { server, ups, action, result } => {
                let i = self.ensure_ups(server, &ups, "");
                let label = self.ups[i].label.clone();
                let what = match &action {
                    Action::Command { .. } => format!("Command {}", action.describe()),
                    Action::Set { .. } => format!("Set {}", action.describe()),
                };
                match result {
                    Ok(msg) => {
                        self.toast(Severity::Good, format!("{what}: {msg}"));
                        self.log_event(&label, Severity::Good, format!("{what}: {msg}"));
                    }
                    Err((err, auth)) => {
                        if auth {
                            self.servers[server].creds = None;
                        }
                        self.toast(Severity::Crit, format!("{what} failed: {err}"));
                        self.log_event(&label, Severity::Crit, format!("{what} failed: {err}"));
                    }
                }
                // Show the effect without waiting for the next poll.
                let _ = self.servers[server].tx.send(Request::Refresh);
            }
        }
    }

    /// Housekeeping before each frame.
    pub fn tick(&mut self) {
        if self.toast.as_ref().is_some_and(|t| t.at.elapsed() > Duration::from_secs(5)) {
            self.toast = None;
        }
        if self.view == View::Variables {
            self.request_info();
        }
    }

    /// Variables shown in the Variables view, after the filter.
    pub fn visible_vars(&self) -> Vec<String> {
        let Some(u) = self.current() else { return Vec::new() };
        let f = self.vars.filter.text.to_lowercase();
        u.vars
            .iter()
            .filter(|(k, v)| f.is_empty() || k.to_lowercase().contains(&f) || v.value.to_lowercase().contains(&f))
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn selected_var(&self) -> Option<String> {
        let vars = self.visible_vars();
        let i = self.vars.table.selected()?.min(vars.len().checked_sub(1)?);
        vars.into_iter().nth(i)
    }

    fn describe(&mut self, var: &str) {
        let Some(u) = self.ups.get_mut(self.sel) else { return };
        if u.info.contains_key(var) || u.info_pending.contains(var) || !u.is_live() {
            return;
        }
        u.info_pending.insert(var.to_string());
        let _ = self.servers[u.server].tx.send(Request::Describe { ups: u.name.clone(), var: var.to_string() });
    }

    fn request_info(&mut self) {
        if let Some(var) = self.selected_var() {
            self.describe(&var);
        }
    }

    fn broadcast(&self, r: impl Fn() -> Request) {
        for s in &self.servers {
            let _ = s.tx.send(r());
        }
    }

    fn set_interval(&mut self, step: isize) {
        let cur = self.interval.as_secs_f64();
        let i = INTERVALS.iter().position(|v| *v >= cur - 1e-9).unwrap_or(INTERVALS.len() - 1);
        let j = (i as isize + step).clamp(0, INTERVALS.len() as isize - 1) as usize;
        // From a custom value, the first step lands on the neighbor.
        let j = if (INTERVALS[i] - cur).abs() > 1e-9 && step > 0 { i } else { j };
        self.interval = Duration::from_secs_f64(INTERVALS[j]);
        let d = self.interval;
        self.broadcast(|| Request::Interval(d));
        self.toast(Severity::Info, format!("Polling every {}", crate::fmt::duration(INTERVALS[j]).replace(" 00s", "")));
    }

    fn select_ups(&mut self, i: usize) {
        if i < self.ups.len() && i != self.sel {
            self.sel = i;
            self.vars.table.select(Some(0));
            self.hist.selected = 0;
            self.hist.zoomed = false;
        }
    }

    fn cycle_ups(&mut self, step: isize) {
        let n = self.ups.len() as isize;
        if n > 1 {
            self.select_ups((self.sel as isize + step).rem_euclid(n) as usize);
        }
    }

    fn set_view(&mut self, v: View) {
        self.view = v;
        self.vars.filtering = false;
    }

    // ---- actions -------------------------------------------------------

    fn start_action(&mut self, ups: usize, action: Action) {
        let server = self.ups[ups].server;
        match self.servers[server].creds.clone() {
            Some(creds) => {
                let name = self.ups[ups].name.clone();
                self.toast(Severity::Info, format!("Sending {} ...", action.describe()));
                let _ = self.servers[server].tx.send(Request::Run { ups: name, action, creds });
                self.dialog = None;
            }
            None => {
                let user = Input::new(self.default_user.as_deref().unwrap_or(""));
                let focus = if user.text.is_empty() { 0 } else { 1 };
                self.dialog =
                    Some(Dialog::Login { server, user, password: Input::default(), focus, then: Some((ups, action)) });
            }
        }
    }

    fn open_commands(&mut self) {
        match self.current() {
            Some(u) if !u.cmds.is_empty() => self.dialog = Some(Dialog::Commands { selected: 0 }),
            Some(_) => self.toast(Severity::Info, "This UPS lists no instant commands"),
            None => {}
        }
    }

    fn open_edit(&mut self) {
        let Some(var) = self.selected_var() else { return };
        let Some(u) = self.current() else { return };
        if !u.rw.contains(&var) {
            self.toast(Severity::Info, format!("{var} is read-only"));
            return;
        }
        let value = u.get(&var).unwrap_or("").to_string();
        let selected = u.info.get(&var).and_then(|i| i.enum_values.iter().position(|v| *v == value)).unwrap_or(0);
        self.describe(&var);
        self.dialog = Some(Dialog::Edit { ups: self.sel, var, input: Input::new(&value), selected, error: None });
    }

    // ---- input ---------------------------------------------------------

    pub fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(k.code, KeyCode::Char('c')) {
            self.quit = true;
            return;
        }
        if self.dialog.is_some() {
            self.dialog_key(k);
            return;
        }
        if self.view == View::Variables && self.vars.filtering {
            match k.code {
                KeyCode::Esc => {
                    self.vars.filter = Input::default();
                    self.vars.filtering = false;
                }
                KeyCode::Enter => self.vars.filtering = false,
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => self.vars_key(k),
                _ => {
                    if self.vars.filter.key(&k) {
                        self.vars.table.select(Some(0));
                    }
                }
            }
            return;
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') | KeyCode::F(1) => self.dialog = Some(Dialog::Help),
            KeyCode::Tab => self.set_view(View::ALL[(self.view.index() + 1) % 4]),
            KeyCode::BackTab => self.set_view(View::ALL[(self.view.index() + 3) % 4]),
            KeyCode::Char(c @ '1'..='4') => self.set_view(View::ALL[c as usize - '1' as usize]),
            KeyCode::Char(']') => self.cycle_ups(1),
            KeyCode::Char('[') => self.cycle_ups(-1),
            KeyCode::Char('r') => {
                self.broadcast(|| Request::Refresh);
                self.toast(Severity::Info, "Refreshing");
            }
            KeyCode::Char('p') => {
                self.paused = !self.paused;
                let p = self.paused;
                self.broadcast(|| Request::Pause(p));
                self.toast(Severity::Info, if p { "Polling paused" } else { "Polling resumed" });
            }
            KeyCode::Char('<') => self.set_interval(-1),
            KeyCode::Char('>') => self.set_interval(1),
            KeyCode::Char('c') => self.open_commands(),
            KeyCode::Char('b') => {
                self.charge_style = self.charge_style.next();
                let s = self.charge_style.name();
                self.toast(Severity::Info, format!("Charge style: {s} (--charge-style {s} keeps it)"));
            }
            _ => match self.view {
                View::Variables => self.vars_key(k),
                View::History => self.hist_key(k),
                View::Events => self.events_key(k),
                View::Overview => {}
            },
        }
    }

    fn vars_key(&mut self, k: KeyEvent) {
        let n = self.visible_vars().len();
        let cur = self.vars.table.selected().unwrap_or(0).min(n.saturating_sub(1));
        let page = 10;
        let to = match k.code {
            KeyCode::Up | KeyCode::Char('k') => cur.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => cur + 1,
            KeyCode::PageUp => cur.saturating_sub(page),
            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => cur.saturating_sub(page),
            KeyCode::PageDown => cur + page,
            KeyCode::Char('d') if k.modifiers.contains(KeyModifiers::CONTROL) => cur + page,
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => n,
            KeyCode::Char('/') => {
                self.vars.filtering = true;
                return;
            }
            KeyCode::Esc => {
                self.vars.filter = Input::default();
                return;
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                self.open_edit();
                return;
            }
            KeyCode::Char('y') => {
                if let (Some(var), Some(u)) = (self.selected_var(), self.current()) {
                    let value = u.get(&var).unwrap_or("").to_string();
                    self.toast(Severity::Info, format!("Copied {var}"));
                    self.clipboard = Some(value);
                }
                return;
            }
            _ => return,
        };
        self.vars.table.select(Some(to.min(n.saturating_sub(1))));
    }

    fn hist_key(&mut self, k: KeyEvent) {
        let n = self.hist.count.max(1);
        match k.code {
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Up | KeyCode::Char('k') => {
                self.hist.selected = (self.hist.selected + n - 1) % n;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Down | KeyCode::Char('j') => {
                self.hist.selected = (self.hist.selected + 1) % n;
            }
            KeyCode::Enter | KeyCode::Char('z') => self.hist.zoomed = !self.hist.zoomed,
            KeyCode::Esc => self.hist.zoomed = false,
            KeyCode::Char('+') | KeyCode::Char('=') => self.zoom(-1),
            KeyCode::Char('-') | KeyCode::Char('_') => self.zoom(1),
            _ => {}
        }
    }

    fn zoom(&mut self, step: isize) {
        let max = WINDOWS.iter().rposition(|w| *w <= self.history_keep.as_secs_f64() + 1.0).unwrap_or(0);
        self.hist.window = (self.hist.window as isize + step).clamp(0, max as isize) as usize;
    }

    fn events_key(&mut self, k: KeyEvent) {
        let n = self.log.len();
        let o = self.events.offset;
        self.events.offset = match k.code {
            KeyCode::Up | KeyCode::Char('k') => o.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => o + 1,
            KeyCode::PageUp => o.saturating_sub(10),
            KeyCode::PageDown => o + 10,
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => n,
            KeyCode::Char('f') => {
                self.events.all = !self.events.all;
                0
            }
            _ => o,
        }
        .min(n.saturating_sub(1));
    }

    fn dialog_key(&mut self, k: KeyEvent) {
        let Some(dialog) = self.dialog.take() else { return };
        self.dialog = match dialog {
            Dialog::Help => None,
            Dialog::Commands { selected } => self.commands_key(k, selected),
            Dialog::Confirm { ups, action, desc, mut typed, error } => {
                let is_dangerous = matches!(&action, Action::Command { cmd, .. } if dangerous(cmd));
                match k.code {
                    KeyCode::Esc => None,
                    KeyCode::Char('n') if !is_dangerous => None,
                    KeyCode::Enter | KeyCode::Char('y') if !is_dangerous => {
                        self.start_action(ups, action);
                        return;
                    }
                    KeyCode::Enter => {
                        let Action::Command { cmd, .. } = &action else { unreachable!() };
                        if typed.text.trim() == cmd {
                            self.start_action(ups, action);
                            return;
                        }
                        let error = Some("type the command name exactly to run it".to_string());
                        Some(Dialog::Confirm { ups, action, desc, typed, error })
                    }
                    _ => {
                        if is_dangerous {
                            typed.key(&k);
                        }
                        Some(Dialog::Confirm { ups, action, desc, typed, error })
                    }
                }
            }
            Dialog::Login { server, mut user, mut password, mut focus, then } => match k.code {
                KeyCode::Esc => None,
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                    focus = 1 - focus;
                    Some(Dialog::Login { server, user, password, focus, then })
                }
                KeyCode::Enter if focus == 0 => Some(Dialog::Login { server, user, password, focus: 1, then }),
                KeyCode::Enter if !user.text.is_empty() => {
                    self.servers[server].creds = Some(Creds { user: user.text, password: password.text });
                    if let Some((ups, action)) = then {
                        self.start_action(ups, action);
                        return;
                    }
                    None
                }
                _ => {
                    if focus == 0 {
                        user.key(&k)
                    } else {
                        password.key(&k)
                    };
                    Some(Dialog::Login { server, user, password, focus, then })
                }
            },
            Dialog::Edit { ups, var, mut input, mut selected, error } => {
                let choices = self.ups[ups].info.get(&var).map(|i| i.enum_values.clone()).unwrap_or_default();
                match k.code {
                    KeyCode::Esc => None,
                    KeyCode::Up | KeyCode::Char('k') if !choices.is_empty() => {
                        selected = selected.saturating_sub(1);
                        Some(Dialog::Edit { ups, var, input, selected, error })
                    }
                    KeyCode::Down | KeyCode::Char('j') if !choices.is_empty() => {
                        selected = (selected + 1).min(choices.len() - 1);
                        Some(Dialog::Edit { ups, var, input, selected, error })
                    }
                    KeyCode::Enter => {
                        let value = if choices.is_empty() {
                            input.text.clone()
                        } else {
                            choices[selected.min(choices.len() - 1)].clone()
                        };
                        match self.validate(ups, &var, &value) {
                            Ok(()) => {
                                self.start_action(ups, Action::Set { var, value });
                                return;
                            }
                            Err(e) => Some(Dialog::Edit { ups, var, input, selected, error: Some(e) }),
                        }
                    }
                    _ => {
                        if choices.is_empty() {
                            input.key(&k);
                        }
                        Some(Dialog::Edit { ups, var, input, selected, error: None })
                    }
                }
            }
        };
    }

    fn commands_key(&mut self, k: KeyEvent, selected: usize) -> Option<Dialog> {
        let n = self.current().map_or(0, |u| u.cmds.len());
        let selected = match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('c') => return None,
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (selected + 1).min(n.saturating_sub(1)),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => n.saturating_sub(1),
            KeyCode::Enter => {
                let u = self.current()?;
                let cmd = u.cmds.get(selected)?;
                return Some(Dialog::Confirm {
                    ups: self.sel,
                    action: Action::Command { cmd: cmd.name.clone(), value: None },
                    desc: cmd.desc.clone(),
                    typed: Input::default(),
                    error: None,
                });
            }
            _ => selected,
        };
        Some(Dialog::Commands { selected })
    }

    fn validate(&self, ups: usize, var: &str, value: &str) -> Result<(), String> {
        let Some(info) = self.ups[ups].info.get(var) else { return Ok(()) };
        if let Some(max) = info.max_len()
            && value.chars().count() > max
        {
            return Err(format!("at most {max} characters"));
        }
        if !info.ranges.is_empty() {
            let v: f64 = value.trim().parse().map_err(|_| "expected a number".to_string())?;
            let ok = info.ranges.iter().any(
                |(lo, hi)| matches!((lo.parse::<f64>(), hi.parse::<f64>()), (Ok(lo), Ok(hi)) if v >= lo && v <= hi),
            );
            if !ok {
                let r: Vec<String> = info.ranges.iter().map(|(a, b)| format!("{a}..{b}")).collect();
                return Err(format!("outside {}", r.join(", ")));
            }
        }
        Ok(())
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        let hit = self.hits.iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, h)| *h);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                let code = if up { KeyCode::Up } else { KeyCode::Down };
                match (&self.dialog, self.view) {
                    (Some(Dialog::Commands { .. } | Dialog::Edit { .. }), _) => self.dialog_key(key(code)),
                    (Some(_), _) => {}
                    (None, View::Variables) => {
                        for _ in 0..3 {
                            self.vars_key(key(code));
                        }
                    }
                    (None, View::Events) => {
                        for _ in 0..3 {
                            self.events_key(key(code));
                        }
                    }
                    (None, View::History) => self.zoom(if up { -1 } else { 1 }),
                    (None, View::Overview) => {}
                }
            }
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(Hit::Tab(v)) if self.dialog.is_none() => self.set_view(v),
                Some(Hit::Ups(i)) if self.dialog.is_none() => self.select_ups(i),
                Some(Hit::VarRow(i)) if self.dialog.is_none() => {
                    if self.vars.table.selected() == Some(i) {
                        self.open_edit();
                    } else {
                        self.vars.table.select(Some(i));
                    }
                }
                Some(Hit::Chart(i)) if self.dialog.is_none() => {
                    if self.hist.selected == i {
                        self.hist.zoomed = !self.hist.zoomed;
                    } else {
                        self.hist.selected = i;
                    }
                }
                Some(Hit::DialogRow(i)) => match &mut self.dialog {
                    Some(Dialog::Commands { selected }) => {
                        if *selected == i {
                            self.dialog_key(key(KeyCode::Enter));
                        } else {
                            *selected = i;
                        }
                    }
                    Some(Dialog::Edit { selected, .. }) => {
                        if *selected == i {
                            self.dialog_key(key(KeyCode::Enter));
                        } else {
                            *selected = i;
                        }
                    }
                    _ => {}
                },
                None if matches!(self.dialog, Some(Dialog::Help)) => self.dialog = None,
                _ => {}
            },
            _ => {}
        }
    }
}
