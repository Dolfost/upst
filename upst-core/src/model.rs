//! What upst knows about one UPS: its variables, how they changed, their
//! history, and the events and estimates derived from that.
//!
//! Nothing here looks at the clock: every call that changes the state
//! takes the time it happened at, so polls, recorded journals and tests
//! all go through the same code.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::event::{Event, EventKind};
use crate::fmt::parse_num;
use crate::history::{Band, History, Tracker, Trend, recordable};
use crate::journal::{GAP, Kind, Record};
use crate::status::{Power, Severity, Status};

/// Unix seconds of a time; 0 before 1970.
pub fn unix(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The time `t` Unix seconds after the epoch. Negative or NaN counts as 0,
/// and anything past the year 5000 as then, which every platform can hold.
pub fn from_unix(t: f64) -> SystemTime {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, MAX_UNIX) };
    UNIX_EPOCH + Duration::from_secs_f64(t)
}

/// About the year 5000.
const MAX_UNIX: f64 = 95_617_584_000.0;

/// One variable of a UPS.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Var {
    /// The value as upsd reports it.
    pub value: String,
    /// The value before the last change, if it changed while watched.
    pub prev: Option<String>,
    /// When the value last changed or appeared; `None` if it has been the
    /// same since the first poll.
    pub changed: Option<SystemTime>,
}

/// How data for a UPS is getting through.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "state", content = "why", rename_all = "lowercase"))]
pub enum Link {
    /// Nothing heard yet.
    Connecting,
    /// Polls succeed.
    Up,
    /// upsd answers, but not with data for this UPS.
    Stale(String),
    /// upsd itself is unreachable.
    Down(String),
}

/// The charge rate and what it implies, see [`Ups::estimate`]. Fields may be
/// added in minor versions.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Estimate {
    /// Which way the charge is going.
    pub trend: Trend,
    /// The charge rate in percent per hour, negative while discharging.
    pub per_hour: f64,
    /// True when the rate rests on a single step of the charge, which
    /// makes it a rough guess.
    pub rough: bool,
    /// Seconds until `battery.charge.low`, while discharging and above
    /// it.
    pub to_low: Option<f64>,
    /// Seconds until the charge reaches 0 %, while discharging.
    pub to_empty: Option<f64>,
    /// Seconds until the charge reaches 100 %, while charging.
    pub to_full: Option<f64>,
    /// The average draw (or charging power) in watts that the rate
    /// implies, when the battery capacity is known.
    pub watts: Option<f64>,
}

/// Builds an [`Ups`], see [`Ups::builder`].
#[derive(Clone, Debug)]
#[must_use]
pub struct UpsBuilder {
    name: String,
    server_label: String,
    capacity: Option<f64>,
    history: Option<History>,
}

impl UpsBuilder {
    /// The battery capacity in watt-hours, for [`Estimate::watts`].
    /// Without one, it is guessed where a driver reports it (EcoFlow).
    pub fn capacity_wh(mut self, wh: Option<f64>) -> Self {
        self.capacity = wh;
        self
    }

    /// Where to keep the history of every numeric variable, or `None`
    /// not to keep one. The default is [`History::default`], an hour.
    /// Estimates work either way.
    pub fn history(mut self, history: Option<History>) -> Self {
        self.history = history;
        self
    }

    /// The UPS, with nothing known yet.
    pub fn build(self) -> Ups {
        Ups {
            label: format!("{}@{}", self.name, self.server_label),
            name: self.name,
            server_label: self.server_label,
            vars: BTreeMap::new(),
            status: Status::default(),
            link: Link::Connecting,
            updated: None,
            history: self.history,
            tracker: Tracker::default(),
            power_since: None,
            outages: 0,
            battery_secs: 0.0,
            outage_start: None,
            capacity_wh: self.capacity,
            capacity_guessed: false,
            capacity_set: self.capacity,
            polls: 0,
            since: None,
            lost: false,
        }
    }
}

/// One UPS: its variables and status, how the connection to it goes,
/// outage statistics, history, events and estimates.
///
/// Feed it every `LIST VAR` with [`apply`](Ups::apply), and connection
/// trouble with [`lost`](Ups::lost); both return the [`Event`]s that
/// implies.
///
/// ```
/// use std::time::{Duration, UNIX_EPOCH};
/// use upst_core::{EventKind, Ups};
///
/// let vars = |status: &str, charge: &str| {
///     vec![("ups.status".to_string(), status.to_string()), ("battery.charge".to_string(), charge.to_string())]
/// };
/// let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
/// let mut ups = Ups::builder("eco", "localhost").history(None).build();
/// assert!(ups.apply(vars("OL", "100"), t0).is_empty());
/// let ev = ups.apply(vars("OB DISCHRG", "100"), t0 + Duration::from_secs(10));
/// assert_eq!(ev[0].code(), "power.lost");
/// let ev = ups.apply(vars("OL CHRG", "97"), t0 + Duration::from_secs(130));
/// assert_eq!(ev[0].kind, EventKind::PowerRestored { on_battery: Duration::from_secs(120) });
/// assert_eq!(ev[0].to_string(), "Power restored after 2m 00s on battery");
/// ```
#[derive(Clone, Debug)]
pub struct Ups {
    name: String,
    server_label: String,
    label: String,
    vars: BTreeMap<String, Var>,
    status: Status,
    link: Link,
    updated: Option<SystemTime>,
    history: Option<History>,
    tracker: Tracker,
    power_since: Option<(SystemTime, bool)>,
    outages: u32,
    battery_secs: f64,
    outage_start: Option<SystemTime>,
    capacity_wh: Option<f64>,
    capacity_guessed: bool,
    /// Set from outside; `None` lets the capacity be guessed.
    capacity_set: Option<f64>,
    polls: u64,
    since: Option<SystemTime>,
    /// upsd was lost since the last poll, so the next one brings it back.
    lost: bool,
}

impl Ups {
    /// Start building a UPS named `name` on the upsd server
    /// `server_label` (e.g. `localhost` or `nas:3500`, as in a target).
    pub fn builder(name: &str, server_label: &str) -> UpsBuilder {
        UpsBuilder {
            name: name.to_string(),
            server_label: server_label.to_string(),
            capacity: None,
            history: Some(History::default()),
        }
    }

    /// A UPS with the defaults of [`Ups::builder`].
    pub fn new(name: &str, server_label: &str) -> Ups {
        Ups::builder(name, server_label).build()
    }

    /// The UPS name on its server.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The server, as given to [`Ups::builder`].
    pub fn server_label(&self) -> &str {
        &self.server_label
    }

    /// `name@server`.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Every variable from the last poll, by name.
    pub fn vars(&self) -> &BTreeMap<String, Var> {
        &self.vars
    }

    /// One variable.
    pub fn var(&self, name: &str) -> Option<&Var> {
        self.vars.get(name)
    }

    /// A variable's value.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(|v| v.value.as_str())
    }

    /// A variable's value, trimmed, if that leaves something.
    pub fn text(&self, name: &str) -> Option<&str> {
        self.get(name).map(str::trim).filter(|s| !s.is_empty())
    }

    /// A variable's value as a number, see [`parse_num`].
    pub fn num(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(parse_num)
    }

    /// Whether any of these variables is there.
    pub fn has_any(&self, names: &[&str]) -> bool {
        names.iter().any(|n| self.vars.contains_key(*n))
    }

    /// Whether any variable starts with `prefix`.
    pub fn has_prefix(&self, prefix: &str) -> bool {
        self.vars.range(prefix.to_string()..).next().is_some_and(|(k, _)| k.starts_with(prefix))
    }

    /// The parsed `ups.status` of the last poll.
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// How data is getting through.
    pub fn link(&self) -> &Link {
        &self.link
    }

    /// Whether polls succeed.
    pub fn is_live(&self) -> bool {
        self.link == Link::Up
    }

    /// The time of the last poll.
    pub fn updated(&self) -> Option<SystemTime> {
        self.updated
    }

    /// The history, unless the UPS was built without one.
    pub fn history(&self) -> Option<&History> {
        self.history.as_ref()
    }

    /// The history of one variable.
    pub fn series(&self, name: &str) -> Option<&crate::history::Series> {
        self.history.as_ref()?.get(name)
    }

    /// When the current power source (see [`Power`]) took over, and
    /// whether that was seen happen (false: it was already so at the first
    /// poll).
    pub fn power_since(&self) -> Option<(SystemTime, bool)> {
        self.power_since
    }

    /// How many times the UPS went on battery since the first poll.
    pub fn outages(&self) -> u32 {
        self.outages
    }

    /// Seconds on battery since the first poll, not counting a running
    /// outage; see [`Ups::battery_total`].
    pub fn battery_secs(&self) -> f64 {
        self.battery_secs
    }

    /// When the running outage started, while on battery.
    pub fn outage_start(&self) -> Option<SystemTime> {
        self.outage_start
    }

    /// The battery capacity in watt-hours: set from outside, or guessed.
    pub fn capacity_wh(&self) -> Option<f64> {
        self.capacity_wh
    }

    /// Whether [`Ups::capacity_wh`] was guessed from what the driver
    /// reports (the EcoFlow's pack size) rather than set.
    pub fn capacity_guessed(&self) -> bool {
        self.capacity_guessed
    }

    /// Set the battery capacity in watt-hours, or `None` to go back to
    /// the guess.
    pub fn set_capacity_wh(&mut self, wh: Option<f64>) {
        self.capacity_set = wh;
        self.capacity_wh = wh;
        self.capacity_guessed = false;
        if wh.is_none() {
            self.guess_capacity();
        }
    }

    /// How many polls were applied.
    pub fn polls(&self) -> u64 {
        self.polls
    }

    /// The time of the first poll.
    pub fn since(&self) -> Option<SystemTime> {
        self.since
    }

    /// The charge tracker behind [`Ups::estimate`].
    pub fn tracker(&self) -> &Tracker {
        &self.tracker
    }

    /// Whether this looks like an EcoFlow power station.
    pub fn ecoflow(&self) -> bool {
        ["device.mfr", "ups.mfr"].iter().any(|k| self.get(k).is_some_and(|m| m.eq_ignore_ascii_case("ecoflow")))
    }

    /// The load in percent: `ups.load`, or else `ups.realpower` against
    /// `ups.realpower.nominal`.
    pub fn load(&self) -> Option<f64> {
        self.num("ups.load").or_else(|| {
            let p = self.num("ups.realpower")?;
            let n = self.num("ups.realpower.nominal").filter(|n| *n > 0.0)?;
            Some(p / n * 100.0)
        })
    }

    /// Seconds in the current power state at `at`, and whether its start
    /// was seen (see [`Ups::power_since`]).
    pub fn power_age(&self, at: SystemTime) -> Option<(f64, bool)> {
        let (since, seen) = self.power_since?;
        let age = at.duration_since(since).ok()?.as_secs_f64();
        Some((age, seen))
    }

    /// Seconds on battery since the first poll, including the running
    /// outage up to `at`.
    pub fn battery_total(&self, at: SystemTime) -> f64 {
        let running = self.outage_start.and_then(|s| at.duration_since(s).ok()).map_or(0.0, |d| d.as_secs_f64());
        self.battery_secs + running
    }

    fn band(&self) -> Band {
        if self.status.has("LB") && self.status.has("OB") {
            Band::Low
        } else {
            match self.status.power() {
                Power::Online => Band::Online,
                Power::Battery => Band::Battery,
                Power::Unknown => Band::NoData,
                _ => Band::Other,
            }
        }
    }

    /// The EcoFlow HID subdriver puts the pack's Wh rating into
    /// `ups.power.nominal`.
    fn guess_capacity(&mut self) {
        if self.capacity_set.is_none() && self.ecoflow() {
            self.capacity_wh = self.num("ups.power.nominal").filter(|w| *w > 0.0);
            self.capacity_guessed = self.capacity_wh.is_some();
        }
    }

    /// Fold in a fresh `LIST VAR` that came at `at`; returns the events
    /// it implies, in order.
    ///
    /// The first poll is where watching starts, so it reports no changes:
    /// only [`EventKind::StartedOnBattery`] when the UPS is already on
    /// battery (critical when the battery is low too). Look at
    /// [`Ups::status`] after it for flags that were already set.
    pub fn apply(&mut self, list: Vec<(String, String)>, at: SystemTime) -> Vec<Event> {
        let first = self.polls == 0;
        let mut events = Vec::new();

        // Variables that changed (or appeared) with this poll.
        let mut changed = Vec::new();
        let mut vars = BTreeMap::new();
        for (k, value) in list {
            let (var, fresh) = match self.vars.remove(&k) {
                Some(old) if old.value == value => (old, false),
                Some(old) => (Var { value, prev: Some(old.value), changed: Some(at) }, true),
                None => (Var { value, prev: None, changed: (!first).then_some(at) }, !first),
            };
            if fresh {
                changed.push(k.clone());
            }
            vars.insert(k, var);
        }
        self.vars = vars;

        if std::mem::take(&mut self.lost) {
            events.push(Event::new(at, EventKind::UpsdBack { server: self.server_label.clone() }));
        }
        if let Link::Stale(why) = &self.link {
            events.push(Event::new(at, EventKind::DataFresh { was: why.clone() }));
        }
        self.link = Link::Up;
        self.updated = Some(at);

        let status = Status::parse(self.get("ups.status").unwrap_or(""));
        if first {
            self.since = Some(at);
            self.power_since = Some((at, false));
            if status.power() == Power::Battery {
                self.outage_start = Some(at);
                let (_, sev) = status.headline();
                events.push(Event { at, severity: sev.max(Severity::Warn), kind: EventKind::StartedOnBattery });
            }
            self.guess_capacity();
        } else {
            self.status_events(&status, at, &mut events);
            let fresh = |k: &str| changed.iter().any(|c| c == k).then(|| self.text(k)).flatten();
            if let Some(text) = fresh("ups.alarm") {
                events.push(Event::new(at, EventKind::Alarm { text: text.to_string() }));
            }
            if let Some(result) = fresh("ups.test.result") {
                events.push(Event::new(at, EventKind::SelfTest { result: result.to_string() }));
            }
        }
        self.status = status;

        let t = unix(at);
        let band = self.band();
        if let Some(h) = &mut self.history {
            for (k, v) in &self.vars {
                if recordable(k)
                    && let Some(x) = parse_num(&v.value)
                {
                    h.record(t, k, x);
                }
            }
            h.band(t, band);
            h.trim(t);
        }

        if let Some(charge) = self.num("battery.charge") {
            // Some UPSes (the EcoFlow among them) never say CHRG; on line
            // below full, a rising charge still counts as charging.
            let trend = if self.status.discharging() {
                Some(Trend::Discharging)
            } else if self.status.charging() || (self.status.power() == Power::Online && charge < 100.0) {
                Some(Trend::Charging)
            } else {
                None
            };
            self.tracker.observe(t, charge, trend);
        }
        self.polls += 1;
        events
    }

    fn status_events(&mut self, new: &Status, at: SystemTime, events: &mut Vec<Event>) {
        let old = &self.status;
        let (op, np) = (old.power(), new.power());
        if op != np {
            self.power_since = Some((at, true));
            if np == Power::Battery {
                self.outages += 1;
                self.outage_start = Some(at);
            } else if op == Power::Battery {
                let on_battery = self.outage_start.take().and_then(|s| at.duration_since(s).ok()).unwrap_or_default();
                self.battery_secs += on_battery.as_secs_f64();
                let kind = match np {
                    Power::Online => EventKind::PowerRestored { on_battery },
                    to => EventKind::LeftBattery { to, on_battery },
                };
                events.push(Event::new(at, kind));
            }
        }
        for t in new.tokens() {
            if !old.has(t)
                && let Some(mut kind) = EventKind::from_flag(t, true)
            {
                if let EventKind::BatteryLow { charge } = &mut kind {
                    *charge = self.num("battery.charge");
                }
                events.push(Event::new(at, kind));
            }
        }
        for t in old.tokens() {
            if !new.has(t)
                && let Some(kind) = EventKind::from_flag(t, false)
            {
                events.push(Event::new(at, kind));
            }
        }
    }

    /// The connection or the data went away at `at`: `link` is
    /// [`Link::Stale`] when upsd answers without data for this UPS, or
    /// [`Link::Down`] when upsd is unreachable. Returns the event that
    /// implies, if any; telling the same thing twice is no event.
    pub fn lost(&mut self, link: Link, at: SystemTime) -> Option<Event> {
        if matches!(link, Link::Down(_)) {
            self.lost = true;
        }
        if self.link == link {
            return None;
        }
        let kind = match (&self.link, &link) {
            (Link::Up, Link::Stale(why)) => Some(EventKind::DataStale { why: why.clone() }),
            (Link::Up | Link::Stale(_), Link::Down(why)) => Some(EventKind::UpsdLost { why: why.clone() }),
            (Link::Connecting, Link::Down(why)) if self.polls == 0 => {
                Some(EventKind::UpsdUnreachable { why: why.clone() })
            }
            (Link::Connecting, Link::Stale(why)) => Some(EventKind::DataStale { why: why.clone() }),
            _ => None,
        };
        if matches!(link, Link::Down(_) | Link::Stale(_)) {
            if let Some(h) = &mut self.history {
                h.band(unix(at), Band::NoData);
            }
            self.tracker.reset();
        }
        self.link = link;
        kind.map(|k| Event::new(at, k))
    }

    /// Nothing is known from `at` on, e.g. because the recorder was not
    /// running. The UPS counts as [`Link::Down`] until the next poll, with
    /// no event.
    pub fn gap(&mut self, at: SystemTime) {
        if let Some(h) = &mut self.history {
            h.band(unix(at), Band::NoData);
        }
        self.tracker.reset();
        self.link = Link::Down("not recorded".into());
    }

    /// The charge rate at `at` and what it implies: the time to low
    /// battery, to empty or to full, and the power when the capacity is
    /// known. `None` until the charge has moved in one direction for at
    /// least one step of `battery.charge`.
    pub fn estimate(&self, at: SystemTime) -> Option<Estimate> {
        let now = unix(at);
        let (rate, rough) = self.tracker.rate(now)?;
        let trend = self.tracker.trend()?;
        let charge = self.num("battery.charge")?;
        let per_hour = rate * 3600.0;
        let mut e = Estimate {
            trend,
            per_hour,
            rough,
            to_low: None,
            to_empty: None,
            to_full: None,
            watts: self.capacity_wh.map(|wh| (per_hour / 100.0 * wh).abs()),
        };
        match trend {
            Trend::Discharging => {
                e.to_empty = Some(charge / -rate);
                if let Some(low) = self.num("battery.charge.low").filter(|l| *l > 0.0 && *l < charge) {
                    e.to_low = Some((charge - low) / -rate);
                }
            }
            Trend::Charging => e.to_full = Some(((100.0 - charge) / rate).max(0.0)),
        }
        Some(e)
    }

    /// Feed in a recorder journal (see [`recorder::Client::get`]) as if
    /// its polls had happened live, up to `until`; later records are left
    /// out. Returns the events it implies, each at its recorded time.
    ///
    /// A silence of more than [`GAP`] seconds, or a recorder restart, is
    /// a [`gap`](Ups::gap).
    ///
    /// [`recorder::Client::get`]: crate::recorder::Client::get
    pub fn replay(&mut self, records: &[Record], until: SystemTime) -> Vec<Event> {
        let until = unix(until);
        let mut events = Vec::new();
        let mut vars: BTreeMap<String, String> = BTreeMap::new();
        // Polls were succeeding at the last record.
        let mut up = false;
        // A poll to apply once its records are all read.
        let mut pending: Option<f64> = None;
        let mut last: Option<f64> = None;
        // Only a loss of upsd in the journal makes a reconnection.
        self.lost = false;
        let apply = |u: &mut Ups, vars: &BTreeMap<String, String>, t: f64, events: &mut Vec<Event>| {
            let list = vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            events.extend(u.apply(list, from_unix(t)));
        };
        for r in records.iter().take_while(|r| r.t < until) {
            // A poll is complete once a record of a later time comes, or
            // one that ends the polls: the recorder writes times in whole
            // seconds, so a poll and a lost connection can share one.
            let ends = matches!(r.kind, Kind::Start | Kind::Down(_) | Kind::NoData(_));
            if let Some(p) = pending.filter(|p| *p != r.t || ends) {
                apply(self, &vars, p, &mut events);
                pending = None;
            }
            // Nothing is known about a silence, but a recorder that kept
            // running (a host that slept) goes on polling after it.
            if let Some(l) = last.filter(|l| up && r.t - l > GAP) {
                self.gap(from_unix(l));
            }
            match &r.kind {
                Kind::Start => {
                    if let Some(l) = last.filter(|_| up) {
                        self.gap(from_unix(l));
                    }
                    up = false;
                }
                Kind::Up => {
                    up = true;
                    pending = Some(r.t);
                }
                Kind::Down(why) | Kind::NoData(why) => {
                    up = false;
                    let link = if matches!(r.kind, Kind::Down(_)) {
                        Link::Down(why.clone())
                    } else {
                        Link::Stale(why.clone())
                    };
                    events.extend(self.lost(link, from_unix(r.t)));
                }
                Kind::Tick => {
                    if up {
                        pending = Some(r.t);
                    }
                }
                Kind::Set(k, v) => {
                    vars.insert(k.clone(), v.clone());
                    if up {
                        pending = Some(r.t);
                    }
                }
                Kind::Unset(k) => {
                    vars.remove(k);
                    if up {
                        pending = Some(r.t);
                    }
                }
            }
            last = Some(r.t);
        }
        if let Some(p) = pending {
            apply(self, &vars, p, &mut events);
        }
        if let Some(l) = last.filter(|l| up && until - l > GAP) {
            self.gap(from_unix(l));
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ups() -> Ups {
        Ups::builder("ups", "localhost").history(Some(History::new(3600.0, 2.0))).build()
    }

    fn vars(status: &str, charge: &str) -> Vec<(String, String)> {
        vec![
            ("ups.status".into(), status.into()),
            ("battery.charge".into(), charge.into()),
            ("battery.charge.low".into(), "20".into()),
        ]
    }

    fn texts(events: &[Event]) -> Vec<String> {
        events.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn outage_events_and_stats() {
        let mut u = ups();
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert!(u.apply(vars("OL", "100"), t0).is_empty());
        let ev = u.apply(vars("OB DISCHRG", "100"), t0 + Duration::from_secs(10));
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].severity, Severity::Warn);
        assert_eq!(ev[0].kind, EventKind::PowerLost);
        assert_eq!(u.outages(), 1);
        let ev = u.apply(vars("OB DISCHRG LB", "19"), t0 + Duration::from_secs(70));
        assert_eq!(texts(&ev), ["Battery low (19%)"]);
        assert_eq!(ev[0].kind, EventKind::BatteryLow { charge: Some(19.0) });
        let ev = u.apply(vars("OL CHRG", "19"), t0 + Duration::from_secs(130));
        assert_eq!(ev[0].kind, EventKind::PowerRestored { on_battery: Duration::from_secs(120) });
        assert!(texts(&ev).iter().any(|e| e.starts_with("Power restored after 2m 00s")));
        assert!((u.battery_secs() - 120.0).abs() < 1e-6);
        assert_eq!(u.var("battery.charge").unwrap().changed, Some(t0 + Duration::from_secs(70)));
        assert!(ev.iter().all(|e| e.at == t0 + Duration::from_secs(130)));
    }

    #[test]
    fn estimates_time_to_low() {
        let mut u = ups();
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        u.apply(vars("OB DISCHRG", "50"), t0);
        for i in 1..=4u64 {
            u.apply(vars("OB DISCHRG", &(50 - i).to_string()), t0 + Duration::from_secs(i * 60));
        }
        let e = u.estimate(t0 + Duration::from_secs(240)).unwrap();
        assert!((e.per_hour + 60.0).abs() < 1e-6);
        // 46 % now, 1 %/min, low at 20 %.
        assert!((e.to_low.unwrap() - 26.0 * 60.0).abs() < 1e-6);
        assert!((e.to_empty.unwrap() - 46.0 * 60.0).abs() < 1e-6);
    }

    #[test]
    fn works_without_history() {
        let mut u = Ups::builder("ups", "localhost").history(None).build();
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        u.apply(vars("OB DISCHRG", "50"), t0);
        u.apply(vars("OB DISCHRG", "49"), t0 + Duration::from_secs(60));
        u.apply(vars("OB DISCHRG", "48"), t0 + Duration::from_secs(120));
        assert!(u.history().is_none() && u.series("battery.charge").is_none());
        assert!(u.estimate(t0 + Duration::from_secs(120)).is_some());
        u.lost(Link::Down("gone".into()), t0 + Duration::from_secs(130));
        u.gap(t0 + Duration::from_secs(140));
    }

    #[test]
    fn capacity_set_or_guessed() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let eco = vec![("device.mfr".to_string(), "EcoFlow".to_string()), ("ups.power.nominal".into(), "2048".into())];
        let mut u = Ups::new("eco", "localhost");
        u.apply(eco.clone(), t0);
        assert_eq!((u.capacity_wh(), u.capacity_guessed()), (Some(2048.0), true));
        u.set_capacity_wh(Some(1000.0));
        assert_eq!((u.capacity_wh(), u.capacity_guessed()), (Some(1000.0), false));
        u.set_capacity_wh(None);
        assert_eq!((u.capacity_wh(), u.capacity_guessed()), (Some(2048.0), true));
        let mut u = Ups::builder("eco", "localhost").capacity_wh(Some(500.0)).build();
        u.apply(eco, t0);
        assert_eq!((u.capacity_wh(), u.capacity_guessed()), (Some(500.0), false));
    }

    fn rec(t: f64, kind: Kind) -> Record {
        Record::new(t, kind)
    }

    fn set(t: f64, k: &str, v: &str) -> Record {
        rec(t, Kind::Set(k.into(), v.into()))
    }

    #[test]
    fn replays_outages_and_gaps() {
        let t0 = 1_700_000_000.0;
        let records = vec![
            rec(t0, Kind::Start),
            rec(t0, Kind::Up),
            set(t0, "ups.status", "OL"),
            set(t0, "battery.charge", "100"),
            set(t0 + 60.0, "ups.status", "OB DISCHRG"),
            set(t0 + 120.0, "battery.charge", "99"),
            set(t0 + 180.0, "battery.charge", "98"),
            set(t0 + 240.0, "ups.status", "OL CHRG"),
            rec(t0 + 300.0, Kind::Tick),
            rec(t0 + 330.0, Kind::Down("Connection refused".into())),
            rec(t0 + 400.0, Kind::Up),
            // The recorder stopped here; it starts again 1000 s later.
            rec(t0 + 420.0, Kind::Tick),
            rec(t0 + 1420.0, Kind::Start),
            rec(t0 + 1420.0, Kind::Up),
            set(t0 + 1420.0, "battery.charge", "100"),
            set(t0 + 1420.0, "ups.status", "OL"),
        ];
        let mut u = Ups::builder("ups", "localhost").history(Some(History::new(86400.0, 2.0))).build();
        let events = u.replay(&records, from_unix(t0 + 1500.0));
        assert_eq!(
            texts(&events),
            [
                "Power failure, running on battery",
                "Power restored after 3m 00s on battery",
                "Lost upsd: Connection refused",
                "Reconnected to localhost"
            ]
        );
        assert_eq!(events[0].at, from_unix(t0 + 60.0));
        assert_eq!(events[3].at, from_unix(t0 + 400.0));
        assert_eq!(u.outages(), 1);
        assert_eq!(u.since(), Some(from_unix(t0)));
        assert_eq!(u.get("battery.charge"), Some("100"));
        assert!(u.is_live());
        let bands = u.history().unwrap().band_columns(t0, t0 + 1500.0, 15, t0 + 1500.0);
        assert_eq!(bands[0], Band::Online);
        assert_eq!(bands[1], Band::Battery);
        assert_eq!(bands[3], Band::NoData);
        assert_eq!(bands[5], Band::NoData);
        assert_eq!(bands[14], Band::Online);
        assert_eq!(u.series("battery.charge").unwrap().at(t0 + 200.0), Some(98.0));

        // Records from `until` on are left to the live polls.
        let mut u = Ups::new("ups", "localhost");
        u.replay(&records, from_unix(t0 + 100.0));
        assert_eq!(u.status().power(), Power::Battery);
    }
}
