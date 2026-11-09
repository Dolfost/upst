//! What upst knows about one UPS: its variables, how they changed,
//! their history, and the events derived from that.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::fmt;
use crate::history::{Band, History, Tracker, Trend};
use crate::status::{Power, Severity, Status, flag_event};

pub fn unix(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn from_unix(t: f64) -> SystemTime {
    UNIX_EPOCH + std::time::Duration::from_secs_f64(t.max(0.0))
}

/// The monotonic instant that matches a wall clock time in the past.
fn instant(at: SystemTime) -> Instant {
    let now = Instant::now();
    match SystemTime::now().duration_since(at) {
        Ok(ago) => now.checked_sub(ago).unwrap_or(now),
        Err(_) => now,
    }
}

#[derive(Clone, Debug)]
pub struct Var {
    pub value: String,
    pub prev: Option<String>,
    /// When the value last changed; `None` if it never did while watched.
    pub changed: Option<Instant>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Connecting,
    Up,
    /// upsd answers, but not with data for this UPS.
    Stale(String),
    /// upsd itself is unreachable.
    Down(String),
}

#[derive(Clone, Debug, Default)]
pub struct VarInfo {
    pub desc: Option<String>,
    /// Words from `GET TYPE`, e.g. `RW`, `ENUM`, `STRING:32`, `NUMBER`.
    pub kind: Vec<String>,
    pub enum_values: Vec<String>,
    pub ranges: Vec<(String, String)>,
}

impl VarInfo {
    pub fn max_len(&self) -> Option<usize> {
        self.kind.iter().find_map(|k| k.strip_prefix("STRING:")?.parse().ok())
    }
}

#[derive(Clone, Debug)]
pub struct CmdInfo {
    pub name: String,
    pub desc: Option<String>,
}

/// Commands that can cut power to the load or deep-cycle the battery get
/// a typed confirmation.
pub fn dangerous(cmd: &str) -> bool {
    cmd.starts_with("shutdown.")
        || cmd.contains("load.off")
        || cmd.contains("killpower")
        || cmd.contains("reboot")
        || cmd.starts_with("calibrate.start")
        || cmd.starts_with("test.battery.start.deep")
        || cmd.starts_with("bypass.start")
}

/// Variables that are numbers but not measurements.
fn recordable(name: &str) -> bool {
    !(name.starts_with("driver.")
        || name.contains("serial")
        || name.contains("version")
        || name.contains("vendorid")
        || name.contains("productid")
        || name.ends_with(".date")
        || name.ends_with(".id"))
}

/// One `LIST VAR` answer and when it came.
pub type Poll = (Vec<(String, String)>, SystemTime);

/// Charge rate and what it implies, see `Ups::estimate`.
#[derive(Clone, Debug)]
pub struct Estimate {
    pub trend: Trend,
    /// Percent per hour, negative while discharging.
    pub per_hour: f64,
    /// Based on one step of the charge only.
    pub rough: bool,
    pub to_low: Option<f64>,
    pub to_empty: Option<f64>,
    pub to_full: Option<f64>,
    /// Average draw (or charge power) implied by the capacity.
    pub watts: Option<f64>,
}

pub struct Ups {
    pub server: usize,
    pub server_label: String,
    pub name: String,
    pub label: String,
    pub desc: String,
    pub vars: BTreeMap<String, Var>,
    pub status: Status,
    pub link: Link,
    pub updated: Option<Instant>,
    pub history: History,
    pub tracker: Tracker,
    /// When the current power source started, and whether upst saw it
    /// start (false: it was already that way when upst connected).
    pub power_since: Option<(SystemTime, bool)>,
    pub outages: u32,
    pub battery_secs: f64,
    pub outage_start: Option<SystemTime>,
    pub cmds: Vec<CmdInfo>,
    pub rw: BTreeSet<String>,
    pub info: HashMap<String, VarInfo>,
    pub info_pending: HashSet<String>,
    /// From --capacity, or guessed for drivers that report it.
    pub capacity_wh: Option<f64>,
    pub capacity_guessed: bool,
    capacity_flag: Option<f64>,
    pub polls: u64,
    /// The first poll (or recorded state) upst saw.
    pub since: Option<SystemTime>,
    /// Polls that arrived while recorded history was on its way; they are
    /// applied after it.
    pub held: Option<Vec<Poll>>,
    /// Where recorded history came from, and how far back it goes.
    pub recorded: Option<(String, SystemTime)>,
}

impl Ups {
    pub fn new(
        server: usize,
        server_label: &str,
        name: &str,
        desc: &str,
        history: History,
        capacity: Option<f64>,
    ) -> Self {
        Ups {
            server,
            server_label: server_label.to_string(),
            name: name.to_string(),
            label: format!("{name}@{server_label}"),
            desc: desc.to_string(),
            vars: BTreeMap::new(),
            status: Status::default(),
            link: Link::Connecting,
            updated: None,
            history,
            tracker: Tracker::default(),
            power_since: None,
            outages: 0,
            battery_secs: 0.0,
            outage_start: None,
            cmds: Vec::new(),
            rw: BTreeSet::new(),
            info: HashMap::new(),
            info_pending: HashSet::new(),
            capacity_wh: capacity,
            capacity_guessed: false,
            capacity_flag: capacity,
            polls: 0,
            since: None,
            held: None,
            recorded: None,
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(|v| v.value.as_str())
    }

    /// A non-empty value.
    pub fn text(&self, name: &str) -> Option<&str> {
        self.get(name).map(str::trim).filter(|s| !s.is_empty())
    }

    pub fn num(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(fmt::parse_num)
    }

    pub fn has_any(&self, names: &[&str]) -> bool {
        names.iter().any(|n| self.vars.contains_key(*n))
    }

    pub fn has_prefix(&self, prefix: &str) -> bool {
        self.vars.range(prefix.to_string()..).next().is_some_and(|(k, _)| k.starts_with(prefix))
    }

    pub fn is_live(&self) -> bool {
        self.link == Link::Up
    }

    pub fn ecoflow(&self) -> bool {
        ["device.mfr", "ups.mfr"].iter().any(|k| self.get(k).is_some_and(|m| m.eq_ignore_ascii_case("ecoflow")))
    }

    /// Load percent, measured or derived from real power.
    pub fn load(&self) -> Option<f64> {
        self.num("ups.load").or_else(|| {
            let p = self.num("ups.realpower")?;
            let n = self.num("ups.realpower.nominal").filter(|n| *n > 0.0)?;
            Some(p / n * 100.0)
        })
    }

    /// Seconds in the current power state.
    pub fn power_age(&self) -> Option<(f64, bool)> {
        let (since, seen) = self.power_since?;
        let age = SystemTime::now().duration_since(since).ok()?.as_secs_f64();
        Some((age, seen))
    }

    /// Battery time this session including a running outage.
    pub fn battery_total(&self) -> f64 {
        let running =
            self.outage_start.and_then(|s| SystemTime::now().duration_since(s).ok()).map_or(0.0, |d| d.as_secs_f64());
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

    /// Fold in a fresh `LIST VAR`; returns the events it implies.
    pub fn apply(&mut self, list: Vec<(String, String)>, now: SystemTime) -> Vec<(Severity, String)> {
        // Recorded polls are replayed with their own time.
        let mono = instant(now);
        let first = self.polls == 0;
        let mut events = Vec::new();

        let mut vars = BTreeMap::new();
        for (k, value) in list {
            let var = match self.vars.remove(&k) {
                Some(old) if old.value == value => old,
                Some(old) => Var { value, prev: Some(old.value), changed: Some(mono) },
                None => Var { value, prev: None, changed: (!first).then_some(mono) },
            };
            vars.insert(k, var);
        }
        self.vars = vars;

        if let Link::Stale(why) = &self.link {
            events.push((Severity::Good, format!("UPS data is fresh again (was: {why})")));
        }
        self.link = Link::Up;
        self.updated = Some(mono);

        let status = Status::parse(self.get("ups.status").unwrap_or(""));
        if first {
            self.since = Some(now);
            self.power_since = Some((now, false));
            if status.power() == Power::Battery {
                self.outage_start = Some(now);
                let (_, sev) = status.headline();
                events.push((sev.max(Severity::Warn), "Running on battery".into()));
            }
            if self.capacity_flag.is_none() && self.ecoflow() {
                // The EcoFlow HID subdriver puts the pack's Wh rating into
                // ups.power.nominal.
                self.capacity_wh = self.num("ups.power.nominal").filter(|w| *w > 0.0);
                self.capacity_guessed = self.capacity_wh.is_some();
            }
        } else {
            self.status_events(&status, now, &mut events);
            // A value that changed (or appeared) during this poll.
            let changed = |k: &str| self.vars.get(k).filter(|v| v.changed == Some(mono)).and_then(|_| self.text(k));
            if let Some(alarm) = changed("ups.alarm") {
                events.push((Severity::Crit, format!("Alarm: {alarm}")));
            }
            if let Some(result) = changed("ups.test.result") {
                events.push((Severity::Info, format!("Self-test: {result}")));
            }
        }
        self.status = status;

        let t = unix(now);
        for (k, v) in &self.vars {
            if recordable(k)
                && let Some(x) = fmt::parse_num(&v.value)
            {
                self.history.record(t, k, x);
            }
        }
        let band = self.band();
        self.history.band(t, band);
        self.history.trim(t);

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

    fn status_events(&mut self, new: &Status, now: SystemTime, events: &mut Vec<(Severity, String)>) {
        let old = &self.status;
        let (op, np) = (old.power(), new.power());
        if op != np {
            self.power_since = Some((now, true));
            if np == Power::Battery {
                self.outages += 1;
                self.outage_start = Some(now);
            } else if op == Power::Battery {
                let secs =
                    self.outage_start.take().and_then(|s| now.duration_since(s).ok()).map_or(0.0, |d| d.as_secs_f64());
                self.battery_secs += secs;
                let what = match np {
                    Power::Online => "Power restored",
                    _ => "Left battery power",
                };
                events.push((Severity::Good, format!("{what} after {} on battery", fmt::duration(secs))));
            }
        }
        for t in new.tokens() {
            if !old.has(t)
                && let Some((sev, mut text)) = flag_event(t, true)
            {
                if t == "LB"
                    && let Some(c) = self.get("battery.charge")
                {
                    text = format!("{text} ({c}%)");
                }
                events.push((sev, text));
            }
        }
        for t in old.tokens() {
            if !new.has(t)
                && let Some(ev) = flag_event(t, false)
            {
                events.push(ev);
            }
        }
    }

    /// The connection or the UPS data went away.
    pub fn lost(&mut self, link: Link) -> Option<(Severity, String)> {
        self.lost_at(link, SystemTime::now())
    }

    pub fn lost_at(&mut self, link: Link, at: SystemTime) -> Option<(Severity, String)> {
        if self.link == link {
            return None;
        }
        let ev = match (&self.link, &link) {
            (Link::Up, Link::Stale(why)) => Some((Severity::Warn, format!("No data: {why}"))),
            (Link::Up | Link::Stale(_), Link::Down(why)) => Some((Severity::Crit, format!("Lost upsd: {why}"))),
            (Link::Connecting, Link::Down(why)) if self.polls == 0 => {
                Some((Severity::Crit, format!("Cannot reach upsd: {why}")))
            }
            (Link::Connecting, Link::Stale(why)) => Some((Severity::Warn, format!("No data: {why}"))),
            _ => None,
        };
        if matches!(link, Link::Down(_) | Link::Stale(_)) {
            self.history.band(unix(at), Band::NoData);
            self.tracker.reset();
        }
        self.link = link;
        ev
    }

    /// Nothing is known from `at` on: the recorder was not running.
    pub fn gap(&mut self, at: SystemTime) {
        self.history.band(unix(at), Band::NoData);
        self.tracker.reset();
        self.link = Link::Down("not recorded".into());
    }

    pub fn estimate(&self, now: f64) -> Option<Estimate> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ups() -> Ups {
        Ups::new(0, "localhost", "ups", "", History::new(3600.0, 2.0), None)
    }

    fn vars(status: &str, charge: &str) -> Vec<(String, String)> {
        vec![
            ("ups.status".into(), status.into()),
            ("battery.charge".into(), charge.into()),
            ("battery.charge.low".into(), "20".into()),
        ]
    }

    #[test]
    fn outage_events_and_stats() {
        let mut u = ups();
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert!(u.apply(vars("OL", "100"), t0).is_empty());
        let ev = u.apply(vars("OB DISCHRG", "100"), t0 + Duration::from_secs(10));
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].0, Severity::Warn);
        assert_eq!(u.outages, 1);
        let ev = u.apply(vars("OB DISCHRG LB", "19"), t0 + Duration::from_secs(70));
        assert!(ev.iter().any(|e| e.1 == "Battery low (19%)"));
        let ev = u.apply(vars("OL CHRG", "19"), t0 + Duration::from_secs(130));
        assert!(ev.iter().any(|e| e.1.starts_with("Power restored after 2m 00s")));
        assert!((u.battery_secs - 120.0).abs() < 1e-6);
        assert!(u.vars["battery.charge"].changed.is_some());
    }

    #[test]
    fn estimates_time_to_low() {
        let mut u = ups();
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        u.apply(vars("OB DISCHRG", "50"), t0);
        for i in 1..=4u64 {
            u.apply(vars("OB DISCHRG", &(50 - i).to_string()), t0 + Duration::from_secs(i * 60));
        }
        let e = u.estimate(unix(t0) + 240.0).unwrap();
        assert!((e.per_hour + 60.0).abs() < 1e-6);
        // 46 % now, 1 %/min, low at 20 %.
        assert!((e.to_low.unwrap() - 26.0 * 60.0).abs() < 1e-6);
        assert!((e.to_empty.unwrap() - 46.0 * 60.0).abs() < 1e-6);
    }
}
