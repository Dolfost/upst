//! Replaying recorder journals into a `Ups`: a hand-written scenario, live
//! polls against the journal the recorder would have written of them,
//! gaps and restarts, snapshots, and journal files.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use upst_core::history::{Band, History};
use upst_core::journal::{self, Feed, GAP, Journal, Kind, Record};
use upst_core::{Event, EventKind, Link, Power, Severity, Ups, Var, from_unix};

const T0: f64 = 1_700_000_000.0;

/// The time `s` seconds after T0.
fn at(s: f64) -> SystemTime {
    from_unix(T0 + s)
}

fn rec(s: f64, kind: Kind) -> Record {
    Record::new(T0 + s, kind)
}

fn set(s: f64, k: &str, v: &str) -> Record {
    rec(s, Kind::Set(k.into(), v.into()))
}

fn down(s: f64, why: &str) -> Record {
    rec(s, Kind::Down(why.into()))
}

fn ups() -> Ups {
    Ups::builder("ups", "localhost").history(Some(History::new(86400.0, 1.0))).build()
}

/// Replay into a fresh UPS up to `until` seconds after T0.
fn replay(records: &[Record], until: f64) -> (Ups, Vec<Event>) {
    let mut u = ups();
    let events = u.replay(records, at(until));
    (u, events)
}

fn codes(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(Event::code).collect()
}

/// The status band at `s` seconds after T0.
fn band(u: &Ups, s: f64) -> Band {
    u.history().unwrap().band_columns(T0 + s - 0.5, T0 + s + 0.5, 1, f64::INFINITY)[0]
}

fn dump(records: &[Record]) -> String {
    records.iter().map(|r| format!("\n  {r}")).collect()
}

/// A directory for one test, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(test: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!("upst-core-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// xorshift64, so the property tests need no dependency and every case
/// can be rerun from its seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng((seed + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15))
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

// The scenario upst's replay was first written against.

fn scenario() -> Vec<Record> {
    vec![
        rec(0.0, Kind::Start),
        rec(0.0, Kind::Up),
        set(0.0, "ups.status", "OL"),
        set(0.0, "battery.charge", "100"),
        set(60.0, "ups.status", "OB DISCHRG"),
        set(120.0, "battery.charge", "99"),
        set(180.0, "battery.charge", "98"),
        set(240.0, "ups.status", "OL CHRG"),
        rec(300.0, Kind::Tick),
        down(330.0, "Connection refused"),
        rec(400.0, Kind::Up),
        // The recorder stopped here; it starts again 1000 s later.
        rec(420.0, Kind::Tick),
        rec(1420.0, Kind::Start),
        rec(1420.0, Kind::Up),
        set(1420.0, "battery.charge", "100"),
        set(1420.0, "ups.status", "OL"),
    ]
}

#[test]
fn replays_outages_and_gaps() {
    let (u, events) = replay(&scenario(), 1500.0);
    let got: Vec<_> = events.iter().map(|e| (e.at, e.severity, e.kind.clone())).collect();
    assert_eq!(
        got,
        [
            (at(60.0), Severity::Warn, EventKind::PowerLost),
            (at(240.0), Severity::Good, EventKind::PowerRestored { on_battery: Duration::from_secs(180) }),
            (at(330.0), Severity::Crit, EventKind::UpsdLost { why: "Connection refused".into() }),
            (at(400.0), Severity::Info, EventKind::UpsdBack { server: "localhost".into() }),
        ]
    );
    assert_eq!(codes(&events), ["power.lost", "power.restored", "upsd.lost", "upsd.back"]);
    let texts: Vec<String> = events.iter().map(Event::to_string).collect();
    assert_eq!(
        texts,
        [
            "Power failure, running on battery",
            "Power restored after 3m 00s on battery",
            "Lost upsd: Connection refused",
            "Reconnected to localhost"
        ]
    );

    assert_eq!((u.outages(), u.battery_secs(), u.outage_start()), (1, 180.0, None));
    assert_eq!(u.since(), Some(at(0.0)));
    assert_eq!(u.power_since(), Some((at(240.0), true)));
    assert!(u.is_live());
    // Polls at 0, 60, 120, 180, 240, the ticks at 300 and 420, and the
    // reconnection at 400 and the restart at 1420.
    assert_eq!((u.polls(), u.updated()), (9, Some(at(1420.0))));
    let charge = Var { value: "100".into(), prev: Some("98".into()), changed: Some(at(1420.0)) };
    assert_eq!(u.var("battery.charge"), Some(&charge));
    assert_eq!(u.var("ups.status").unwrap().prev.as_deref(), Some("OL CHRG"));
    // The restart started the estimate over.
    assert_eq!(u.estimate(at(1500.0)), None);

    use Band::*;
    let bands = [(0.0, Online), (59.0, Online), (60.0, Battery), (239.0, Battery), (240.0, Online), (329.0, Online)];
    let more = [(330.0, NoData), (399.0, NoData), (400.0, Online), (419.0, Online), (420.0, NoData), (1419.0, NoData)];
    for (s, want) in bands.into_iter().chain(more).chain([(1420.0, Online), (1499.0, Online)]) {
        assert_eq!(band(&u, s), want, "at {s}");
    }
    let columns = u.history().unwrap().band_columns(T0, T0 + 1500.0, 15, T0 + 1500.0);
    assert_eq!(
        columns,
        [
            Online, Battery, Online, NoData, NoData, NoData, NoData, NoData, NoData, NoData, NoData, NoData, NoData,
            NoData, Online
        ]
    );
    let series = u.series("battery.charge").unwrap();
    for (s, want) in [(0.0, 100.0), (119.0, 100.0), (120.0, 99.0), (200.0, 98.0), (400.0, 98.0), (1420.0, 100.0)] {
        assert_eq!(series.at(T0 + s), Some(want), "at {s}");
    }
    assert_eq!(series.at(T0 - 1.0), None);
    assert!(u.series("ups.status").is_none());

    // Records from `until` on are left to the live polls.
    let (u, events) = replay(&scenario(), 60.0);
    assert_eq!((u.status().power(), events.len()), (Power::Online, 0));
    let (u, events) = replay(&scenario(), 100.0);
    assert_eq!(u.status().power(), Power::Battery);
    assert_eq!(codes(&events), ["power.lost"]);
    assert_eq!(u.battery_total(at(100.0)), 40.0);
}

#[test]
fn replays_without_history() {
    let (with, events) = replay(&scenario(), 1500.0);
    let mut u = Ups::builder("ups", "localhost").history(None).build();
    assert_eq!(u.replay(&scenario(), at(1500.0)), events);
    assert!(u.history().is_none() && u.series("battery.charge").is_none());
    assert_eq!(u.vars(), with.vars());
    assert_eq!((u.status(), u.link(), u.outages(), u.polls()), (with.status(), with.link(), 1, 9));
    for s in [100.0, 200.0, 1500.0] {
        let (u, _) = replay(&scenario(), s);
        let mut bare = Ups::builder("ups", "localhost").history(None).build();
        bare.replay(&scenario(), at(s));
        assert_eq!(bare.estimate(at(s)), u.estimate(at(s)), "at {s}");
    }
    assert!(replay(&scenario(), 200.0).0.estimate(at(200.0)).is_some());
}

// Live polls against their journal.
//
// A run of polls and lost connections is fed (a) live into a `Ups`, and
// (b) into a `Journal` the way `upst record` writes it (with a `!tick`
// check every second), read back from the file and replayed into a fresh
// `Ups`. Both must end up the same, but for what the journal cannot know:
//
// - Polls that changed nothing are not written, and ticks are replayed as
//   polls, so the number and times of polls (`polls`, `updated`) differ,
//   and so do the times of the history points within a run of equal
//   values. The value in effect at any time is the same.
// - A loss of the connection that is already noted is not written again,
//   so the journal keeps the reason of the first: the replayed `link` and
//   the `DataFresh { was }` after it say the first reason, where live
//   says the latest. `as_kept` rewrites a run that way before comparing.
// - Times are whole seconds, and the records of one second are one poll:
//   two polls in one second are merged, and a fractional time is rounded
//   down (`two_polls_in_one_second_merge`, `times_are_whole_seconds`). The
//   runs use whole seconds and at most one poll per second; a poll and a
//   lost connection in the same second replay in the order they happened.
//
// Replay emits `UpsdBack` on the first poll after a `!down`, exactly as
// live `apply` does after `lost(Down)`, so that is no difference.

#[derive(Clone, Debug)]
enum Op {
    Poll(Vec<(String, String)>),
    Down(String),
    NoData(String),
    /// The recorder stops and starts again: nothing live, so only for
    /// journals.
    Restart,
}

const STATUSES: &[&str] = &[
    "OL",
    "OL CHRG",
    "OB DISCHRG",
    "OB DISCHRG LB",
    "OB",
    "OL CHRG LB",
    "OL BOOST",
    "OL TRIM",
    "OL RB",
    "OL OVER",
    "OL CAL",
    "OL ALARM",
    "OL HB ECO",
    "BYPASS",
    "OFF",
    "OB DISCHRG LB FSD",
    "OL NOCOMM",
    "",
];
const ALARMS: &[&str] = &["Replace battery!", "Fan failure\nCheck the unit", "", "C:\\ups\\log", "  padded  "];
const RESULTS: &[&str] = &["Done and passed", "Done and warning", "In progress", "No test initiated"];
const DOWN: &[&str] = &["Connection refused", "Connection reset by peer", "timed out", ""];
const NODATA: &[&str] = &["Driver not connected", "Data stale", "Unknown UPS"];

/// Change some variables the way a UPS might between two polls, and list
/// them all.
fn drift(rng: &mut Rng, vars: &mut BTreeMap<String, String>, base: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut put = |k: &str, v: String| vars.insert(k.to_string(), v);
    if rng.chance(25) {
        put("ups.status", rng.pick(STATUSES).to_string());
    }
    if let Some(c) = vars.get("battery.charge").and_then(|c| c.parse::<i64>().ok())
        && rng.chance(50)
    {
        let step = rng.below(3) as i64;
        let on_battery = vars.get("ups.status").is_some_and(|s| s.contains("OB"));
        let c = if on_battery { c - step } else { c + step };
        vars.insert("battery.charge".into(), c.clamp(0, 100).to_string());
    }
    if rng.chance(40) {
        vars.insert("input.voltage".into(), format!("{}.{}", 200 + rng.below(40), rng.below(10)));
    }
    if rng.chance(8) {
        vars.insert("ups.alarm".into(), rng.pick(ALARMS).to_string());
    } else if rng.chance(5) {
        vars.remove("ups.alarm");
    }
    if rng.chance(4) {
        vars.insert("ups.test.result".into(), rng.pick(RESULTS).to_string());
    }
    if rng.chance(5) && !vars.is_empty() {
        let k = vars.keys().nth(rng.below(vars.len() as u64) as usize).unwrap().clone();
        vars.remove(&k);
    }
    if rng.chance(10) {
        let (k, v) = base.iter().nth(rng.below(base.len() as u64) as usize).unwrap();
        vars.entry(k.clone()).or_insert_with(|| v.clone());
    }
    vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// A pseudo-random run of polls and lost connections at whole seconds,
/// less than `GAP` apart, with sometimes a loss in the same second as a
/// poll, but never two polls in one second.
fn run(rng: &mut Rng, restarts: bool) -> Vec<(f64, Op)> {
    let mfr = if rng.chance(30) { "EcoFlow" } else { "Demo" };
    let base: BTreeMap<String, String> = [
        ("ups.status", "OL"),
        ("battery.charge", "100"),
        ("battery.charge.low", "20"),
        ("input.voltage", "230.0"),
        ("ups.test.result", "No test initiated"),
        ("driver.version", "2.8.4"),
        ("device.mfr", mfr),
        ("ups.power.nominal", "2048"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let mut vars = base.clone();
    let mut t = T0 + rng.below(3) as f64;
    let mut out: Vec<(f64, Op)> = Vec::new();
    let mut lost = false;
    for _ in 0..20 + rng.below(180) {
        if out.is_empty() || !rng.chance(5) {
            t += match rng.below(100) {
                0..70 => 1 + rng.below(10),
                70..92 => 11 + rng.below(80),
                _ => 91 + rng.below(59),
            } as f64;
        }
        let polled = matches!(out.last(), Some((last, Op::Poll(_))) if *last == t);
        let op = if restarts && rng.chance(3) {
            t += rng.below(2000) as f64;
            Op::Restart
        } else if polled || rng.chance(if lost { 40 } else { 12 }) {
            lost = true;
            if rng.chance(50) { Op::Down(rng.pick(DOWN).to_string()) } else { Op::NoData(rng.pick(NODATA).to_string()) }
        } else {
            lost = false;
            Op::Poll(drift(rng, &mut vars, &base))
        };
        out.push((t, op));
    }
    out
}

/// A run as the journal keeps it: a loss already noted keeps the reason
/// it was noted with.
fn as_kept(ops: &[(f64, Op)]) -> Vec<(f64, Op)> {
    let mut noted: Option<Op> = None;
    let mut out = Vec::new();
    for (t, op) in ops {
        let op = match (&noted, op) {
            (Some(Op::Down(why)), Op::Down(_)) => Op::Down(why.clone()),
            (Some(Op::NoData(why)), Op::NoData(_)) => Op::NoData(why.clone()),
            (_, Op::Down(_) | Op::NoData(_)) => {
                noted = Some(op.clone());
                op.clone()
            }
            _ => {
                noted = None;
                op.clone()
            }
        };
        out.push((*t, op));
    }
    out
}

fn live(ops: &[(f64, Op)], history: bool) -> (Ups, Vec<Event>) {
    let mut u = Ups::builder("ups", "localhost").history(history.then(|| History::new(86400.0, 1.0))).build();
    let mut events = Vec::new();
    for (t, op) in ops {
        let at = from_unix(*t);
        match op {
            Op::Poll(vars) => events.extend(u.apply(vars.clone(), at)),
            Op::Down(why) => events.extend(u.lost(Link::Down(why.clone()), at)),
            Op::NoData(why) => events.extend(u.lost(Link::Stale(why.clone()), at)),
            Op::Restart => panic!("a restart has no live counterpart"),
        }
    }
    (u, events)
}

/// Write a run into a journal in `dir` the way `upst record` does, with
/// a tick every second while it runs, and read the file back.
fn record(dir: &Path, label: &str, ops: &[(f64, Op)]) -> Vec<Record> {
    let mut prev = ops[0].0;
    let mut j = Journal::open(dir, label, prev).unwrap();
    for (t, op) in ops {
        if let Op::Restart = op {
            drop(j);
            j = Journal::open(dir, label, *t).unwrap();
        } else {
            for s in prev as i64 + 1..*t as i64 {
                j.tick(s as f64).unwrap();
            }
            match op {
                Op::Poll(vars) => j.poll(*t, vars),
                Op::Down(why) => j.down(*t, why),
                Op::NoData(why) => j.no_data(*t, why),
                Op::Restart => unreachable!(),
            }
            .unwrap();
        }
        j.tick(*t).unwrap();
        prev = *t;
    }
    let path = j.path.clone();
    drop(j);
    journal::read(&path).unwrap()
}

/// Asserts that two UPSes fed the same polls agree on everything but the
/// number and times of the polls, between `from` and `to`.
#[track_caller]
fn assert_same(a: &Ups, b: &Ups, from: f64, to: f64, ctx: &dyn Fn() -> String) {
    assert_eq!(a.vars(), b.vars(), "vars{}", ctx());
    assert_eq!((a.status(), a.link()), (b.status(), b.link()), "status{}", ctx());
    let outages = |u: &Ups| (u.outages(), u.battery_secs(), u.outage_start(), u.since(), u.power_since());
    assert_eq!(outages(a), outages(b), "outages{}", ctx());
    assert_eq!((a.capacity_wh(), a.capacity_guessed()), (b.capacity_wh(), b.capacity_guessed()), "capacity{}", ctx());
    for s in [to, to + 30.0, to + 1e5] {
        assert_eq!(a.estimate(from_unix(s)), b.estimate(from_unix(s)), "estimate at {s}{}", ctx());
    }
    let (Some(ha), Some(hb)) = (a.history(), b.history()) else {
        return assert_eq!(a.history().is_some(), b.history().is_some());
    };
    let names = |h: &History| {
        let mut n: Vec<String> = h.names().map(str::to_string).collect();
        n.sort();
        n
    };
    assert_eq!(names(ha), names(hb), "series{}", ctx());
    for n in names(ha) {
        let (sa, sb) = (ha.get(&n).unwrap(), hb.get(&n).unwrap());
        for s in from as i64..=to as i64 {
            assert_eq!(sa.at(s as f64), sb.at(s as f64), "{n} at {s}{}", ctx());
        }
    }
    let n = (to - from) as usize + 1;
    assert_eq!(
        ha.band_columns(from, to + 1.0, n, to + 1.0),
        hb.band_columns(from, to + 1.0, n, to + 1.0),
        "bands{}",
        ctx()
    );
}

#[test]
fn live_and_replay_agree() {
    let dir = TempDir::new("live_and_replay_agree");
    // How often the runs hit what matters, so a change to the generator
    // cannot quietly stop testing it.
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for case in 0..300 {
        let mut rng = Rng::new(case);
        let ops = run(&mut rng, false);
        let records = record(&dir.0, &format!("ups{case}@localhost"), &ops);
        let ctx = || format!(" in case {case}:{}", dump(&records));
        let (from, to) = (ops[0].0, ops.last().unwrap().0);
        let until = from_unix(to + 1.0);

        let kept = as_kept(&ops);
        let (want, events) = live(&kept, true);
        let mut u = ups();
        assert_eq!(u.replay(&records, until), events, "events{}", ctx());
        assert_same(&want, &u, from, to, &ctx);

        // The run as it happened differs in the reasons only.
        let (raw, raw_events) = live(&ops, true);
        let what = |ev: &[Event]| ev.iter().map(|e| (e.at, e.severity, e.code())).collect::<Vec<_>>();
        assert_eq!(what(&raw_events), what(&events), "raw events{}", ctx());
        assert_eq!(std::mem::discriminant(raw.link()), std::mem::discriminant(want.link()));
        assert_eq!(raw.vars(), want.vars());

        // And without a history, the same again.
        let mut bare = Ups::builder("ups", "localhost").history(None).build();
        assert_eq!(bare.replay(&records, until), events, "events without history{}", ctx());
        assert_same(&live(&kept, false).0, &bare, from, to, &ctx);

        let mut count = |what| *seen.entry(what).or_default() += 1;
        events.iter().for_each(|e| count(e.code()));
        for w in ops.windows(2) {
            if let [(a, Op::Poll(_)), (b, Op::Down(_) | Op::NoData(_))] = w
                && a == b
            {
                count("poll and loss in one second");
            }
        }
        for (a, b) in ops.iter().zip(&kept) {
            if format!("{:?}", a.1) != format!("{:?}", b.1) {
                count("reason kept");
            }
        }
        for r in &records {
            match &r.kind {
                Kind::Tick => count("tick"),
                Kind::Unset(_) => count("unset"),
                Kind::Set(_, v) if v.contains(['\n', '\\']) => count("escaped"),
                _ => {}
            }
        }
    }
    let mins = [
        ("power.lost", 100),
        ("power.restored", 100),
        ("power.started_on_battery", 10),
        ("battery.low", 50),
        ("ups.alarm", 50),
        ("ups.self_test", 50),
        ("data.stale", 100),
        ("data.fresh", 100),
        ("upsd.lost", 100),
        ("upsd.back", 100),
        ("upsd.unreachable", 5),
        ("poll and loss in one second", 30),
        ("reason kept", 30),
        ("tick", 100),
        ("unset", 100),
        ("escaped", 30),
    ];
    for (what, min) in mins {
        assert!(seen.get(what).copied().unwrap_or(0) >= min, "{what}: {seen:?}");
    }
}

// Snapshots: `journal::since(records, t)` is what the recorder serves
// for a window from `t` on. Replaying it must give the state replaying
// the whole journal gives, but for the history before `t`:
//
// - Outage counts and times, `since`, `power_since`, the capacity guess
//   (made at the first poll), estimates, events before the first poll,
//   and the `prev`/`changed` of variables that did not change since are
//   all from before `t` and differ.
// - Durations in `PowerRestored` and `LeftBattery` count from the first
//   poll of the window when the outage started before it.
// - Replay applies variables only with polls. When polls were not
//   running at `t` (the snapshot ends in `!down`, `!nodata` or a
//   `!start`) and never run again, the window has seen no poll: no
//   variables, where the whole journal still has the last ones. After a
//   bare `!start` its link is `Connecting`, where the whole journal has
//   the gap or the loss before the restart.
//
// Everything else (variables, status, link, and the events after the
// window's first poll) must match.

#[test]
fn replaying_since_a_cut_matches_replaying_all() {
    let dir = TempDir::new("replaying_since_a_cut");
    let (mut with_polls, mut without) = (0, 0);
    for case in 0..200 {
        let mut rng = Rng::new(10_000 + case);
        let mut ops = run(&mut rng, true);
        // End some journals with polls not running: upsd gone, no data,
        // or a restart with nothing after it.
        let end = ops.last().unwrap().0;
        match rng.below(3) {
            0 => ops.extend([(end + 5.0, Op::Down("refused".into())), (end + 30.0, Op::NoData("stale".into()))]),
            1 => ops.push((end + 500.0, Op::Restart)),
            _ => {}
        }
        let records = record(&dir.0, &format!("ups{case}@localhost"), &ops);
        let (first, end) = (records[0].t, records.last().unwrap().t + 1.0);
        let mut all = ups();
        let all_events = all.replay(&records, from_unix(end));
        let mut state = journal::State::default();
        records.iter().for_each(|r| state.apply(r));

        let cuts: Vec<f64> = (0..10).map(|_| first - 5.0 + rng.below((end - first) as u64 + 10) as f64).collect();
        let tail = records.iter().rev().take(3).map(|r| r.t + 1.0);
        let cuts = cuts.into_iter().chain(records.iter().step_by(17).map(|r| r.t)).chain(tail);
        for cut in cuts {
            let part = journal::since(records.iter().cloned(), cut);
            let ctx = format!(" in case {case} cut at {cut}:{}", dump(&part));
            let mut u = ups();
            let events = u.replay(&part, from_unix(end));
            if !part.iter().any(|r| r.kind == Kind::Up) {
                without += 1;
                assert!(u.polls() == 0 && u.vars().is_empty() && !u.is_live() && !all.is_live(), "{ctx}");
                if matches!(state.feed, Feed::Down(_) | Feed::NoData(_)) {
                    assert_eq!(u.link(), all.link(), "{ctx}");
                }
                continue;
            }
            with_polls += 1;
            let values = |u: &Ups| u.vars().iter().map(|(k, v)| (k.clone(), v.value.clone())).collect::<Vec<_>>();
            assert_eq!(values(&u), values(&all), "vars{ctx}");
            assert_eq!((u.status(), u.link()), (all.status(), all.link()), "{ctx}");
            for (k, v) in u.vars().iter().filter(|(_, v)| v.changed.is_some()) {
                assert_eq!(Some(v), all.var(k), "{k}{ctx}");
            }
            let first_poll = u.since().unwrap();
            let after = |ev: &[Event]| {
                ev.iter().filter(|e| e.at > first_poll).map(|e| (e.at, e.severity, e.code())).collect::<Vec<_>>()
            };
            assert_eq!(after(&events), after(&all_events), "events{ctx}");
        }
    }
    assert!(with_polls > 1000 && without > 200, "{with_polls} cuts with polls, {without} without");
}

// Gaps and restarts.

/// Discharging 1 % a minute from 50 % at 0 s to 46 % at 240 s.
fn discharging() -> Vec<Record> {
    let mut r = vec![
        rec(0.0, Kind::Start),
        rec(0.0, Kind::Up),
        set(0.0, "ups.status", "OB DISCHRG"),
        set(0.0, "battery.charge", "50"),
    ];
    r.extend((1..=4).map(|i| set(i as f64 * 60.0, "battery.charge", &(50 - i).to_string())));
    r
}

#[test]
fn a_long_silence_is_a_gap() {
    let mut records = discharging();
    let (u, events) = replay(&records, 241.0);
    assert_eq!(codes(&events), ["power.started_on_battery"]);
    assert!(u.estimate(at(241.0)).is_some());
    // The recorder was off for 200 s; it writes what changed meanwhile.
    records.extend([rec(440.0, Kind::Start), rec(440.0, Kind::Up), set(440.0, "battery.charge", "40")]);
    let (u, events) = replay(&records, 450.0);
    assert_eq!(codes(&events), ["power.started_on_battery"]);
    assert!(u.is_live());
    for (s, want) in [(239.0, Band::Battery), (240.0, Band::NoData), (439.0, Band::NoData), (440.0, Band::Battery)] {
        assert_eq!(band(&u, s), want, "at {s}");
    }
    // The estimate starts over: 46 % to 40 % across the gap is no rate.
    assert_eq!(u.estimate(at(440.0)), None);
    records.push(set(500.0, "battery.charge", "39"));
    let (u, _) = replay(&records, 510.0);
    let e = u.estimate(at(500.0)).unwrap();
    assert!(e.rough && (e.per_hour + 60.0).abs() < 1e-9, "{e:?}");
    assert_eq!(u.outages(), 0);
}

#[test]
fn a_restart_is_a_gap() {
    let mut records =
        vec![rec(0.0, Kind::Start), rec(0.0, Kind::Up), set(0.0, "ups.status", "OL"), rec(60.0, Kind::Tick)];
    records.push(rec(100.0, Kind::Start));
    let (u, events) = replay(&records, 120.0);
    assert!(events.is_empty());
    assert_eq!(u.link(), &Link::Down("not recorded".into()));
    assert_eq!((band(&u, 59.0), band(&u, 60.0), band(&u, 110.0)), (Band::Online, Band::NoData, Band::NoData));
    assert_eq!(u.get("ups.status"), Some("OL"));

    records.push(rec(100.0, Kind::Up));
    let (u, events) = replay(&records, 120.0);
    assert!(events.is_empty() && u.is_live());
    assert_eq!((band(&u, 60.0), band(&u, 100.0)), (Band::NoData, Band::Online));
    assert_eq!(u.polls(), 3);
}

#[test]
fn a_short_silence_is_not() {
    let records = [
        rec(0.0, Kind::Start),
        rec(0.0, Kind::Up),
        set(0.0, "ups.status", "OL"),
        set(0.0, "battery.charge", "100"),
        set(GAP, "battery.charge", "99"),
    ];
    let (u, _) = replay(&records, GAP + 10.0);
    assert!(u.is_live());
    for s in [0.0, GAP / 2.0, GAP - 1.0, GAP, GAP + 5.0] {
        assert_eq!(band(&u, s), Band::Online, "at {s}");
    }
    assert_eq!(u.get("battery.charge"), Some("99"));
    // Nor up to `until`.
    let (u, _) = replay(&records[..4], GAP);
    assert!(u.is_live());
}

#[test]
fn a_trailing_silence_is_a_gap() {
    let records = [rec(0.0, Kind::Start), rec(0.0, Kind::Up), set(0.0, "ups.status", "OL")];
    let (u, events) = replay(&records, GAP + 1.0);
    assert!(events.is_empty());
    assert_eq!(u.link(), &Link::Down("not recorded".into()));
    assert_eq!((u.polls(), u.get("ups.status")), (1, Some("OL")));
    assert_eq!(band(&u, 1.0), Band::NoData);
    assert_eq!(u.estimate(at(GAP)), None);

    // While upsd is down it is no gap: it stays down for a reason.
    let mut records = records.to_vec();
    records.push(down(10.0, "Connection refused"));
    let (u, _) = replay(&records, 5000.0);
    assert_eq!(u.link(), &Link::Down("Connection refused".into()));
}

#[test]
fn records_from_until_on_are_left_out() {
    let records = [
        rec(0.0, Kind::Start),
        rec(0.0, Kind::Up),
        set(0.0, "ups.status", "OL"),
        set(0.0, "battery.charge", "100"),
        set(60.0, "ups.status", "OB DISCHRG"),
        set(60.0, "battery.charge", "90"),
        set(120.0, "ups.status", "OL CHRG"),
    ];
    let cases: [(f64, Power, &str, &[&str], f64); 4] = [
        (60.0, Power::Online, "100", &[], 0.0),
        (60.5, Power::Battery, "90", &["power.lost"], 60.0),
        (120.0, Power::Battery, "90", &["power.lost"], 60.0),
        (121.0, Power::Online, "90", &["power.lost", "power.restored"], 120.0),
    ];
    for (until, power, charge, want, updated) in cases {
        let (u, events) = replay(&records, until);
        assert_eq!((u.status().power(), u.get("battery.charge")), (power, Some(charge)), "until {until}");
        assert_eq!(codes(&events), want, "until {until}");
        assert_eq!(u.updated(), Some(at(updated)), "until {until}");
    }
    // Before the first record there is nothing.
    let (u, events) = replay(&records, 0.0);
    assert!(events.is_empty() && u.polls() == 0 && *u.link() == Link::Connecting);
}

#[test]
fn no_data_then_up_is_fresh_data() {
    let records = [
        rec(0.0, Kind::Start),
        rec(0.0, Kind::Up),
        set(0.0, "ups.status", "OL"),
        rec(10.0, Kind::NoData("Driver not connected".into())),
        rec(20.0, Kind::Up),
    ];
    let (u, events) = replay(&records, 30.0);
    let got: Vec<_> = events.iter().map(|e| (e.at, e.severity, e.kind.clone())).collect();
    assert_eq!(
        got,
        [
            (at(10.0), Severity::Warn, EventKind::DataStale { why: "Driver not connected".into() }),
            (at(20.0), Severity::Good, EventKind::DataFresh { was: "Driver not connected".into() }),
        ]
    );
    assert!(u.is_live());
    assert_eq!((band(&u, 9.0), band(&u, 10.0), band(&u, 20.0)), (Band::Online, Band::NoData, Band::Online));
}

#[test]
fn down_then_restart_then_up_is_upsd_back() {
    let records = [
        rec(0.0, Kind::Start),
        rec(0.0, Kind::Up),
        set(0.0, "ups.status", "OL"),
        down(10.0, "Connection refused"),
        rec(20.0, Kind::Start),
        rec(20.0, Kind::Up),
    ];
    let (u, events) = replay(&records, 30.0);
    assert_eq!(codes(&events), ["upsd.lost", "upsd.back"]);
    assert_eq!(events[1].at, at(20.0));
    assert!(u.is_live());

    // Unreachable from the start, and the restarted recorder notes it
    // again: one event, then the reconnection.
    let records = [
        rec(0.0, Kind::Start),
        down(0.0, "Connection refused"),
        rec(30.0, Kind::Start),
        down(30.0, "Connection refused"),
        rec(60.0, Kind::Up),
        set(60.0, "ups.status", "OL"),
    ];
    let (u, events) = replay(&records, 70.0);
    let got: Vec<_> = events.iter().map(|e| (e.at, e.severity, e.code())).collect();
    assert_eq!(got, [(at(0.0), Severity::Crit, "upsd.unreachable"), (at(60.0), Severity::Info, "upsd.back")]);
    assert_eq!(u.since(), Some(at(60.0)));
}

#[test]
fn a_loss_before_the_replay_is_no_reconnection() {
    let mut u = ups();
    u.lost(Link::Down("refused".into()), at(-10.0));
    let events = u.replay(&[rec(0.0, Kind::Start), rec(0.0, Kind::Up), set(0.0, "ups.status", "OL")], at(10.0));
    assert!(events.is_empty() && u.is_live());
}

#[test]
fn unset_removes_a_variable() {
    let records = [
        rec(0.0, Kind::Start),
        rec(0.0, Kind::Up),
        set(0.0, "ups.status", "OL"),
        set(0.0, "ups.alarm", "Overheat"),
        rec(10.0, Kind::Unset("ups.alarm".into())),
        set(20.0, "ups.alarm", "Overheat"),
    ];
    let (u, events) = replay(&records, 15.0);
    assert_eq!((u.get("ups.alarm"), u.get("ups.status")), (None, Some("OL")));
    assert!(events.is_empty());
    // Back again it is new, and an alarm.
    let (u, events) = replay(&records, 25.0);
    let alarm = Var { value: "Overheat".into(), prev: None, changed: Some(at(20.0)) };
    assert_eq!(u.var("ups.alarm"), Some(&alarm));
    assert_eq!(
        events.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
        [EventKind::Alarm { text: "Overheat".into() }]
    );
}

#[test]
fn ticks_keep_the_polls_alive() {
    let mut records = vec![rec(0.0, Kind::Start), rec(0.0, Kind::Up), set(0.0, "ups.status", "OL")];
    let (u, _) = replay(&records, 600.0);
    assert!(!u.is_live());
    records.extend((1..=10).map(|i| rec(i as f64 * 60.0, Kind::Tick)));
    let (u, events) = replay(&records, 601.0);
    assert!(events.is_empty() && u.is_live());
    assert_eq!((u.polls(), u.updated()), (11, Some(at(600.0))));
    assert!(u.history().unwrap().band_columns(T0, T0 + 600.0, 60, T0 + 601.0).iter().all(|b| *b == Band::Online));
    // Ticks while down are no polls.
    records.push(down(610.0, "refused"));
    records.extend((1..=5).map(|i| rec(610.0 + i as f64 * 60.0, Kind::Tick)));
    let (u, _) = replay(&records, 1000.0);
    assert_eq!((u.polls(), u.link()), (11, &Link::Down("refused".into())));
}

#[test]
fn polls_resume_after_a_silence_without_a_restart() {
    // The recorder kept running but wrote nothing for 1000 s: the machine
    // was suspended, or the clock stepped forward. The journal still says
    // polls succeed, so the records after the silence are polls.
    let records = [
        rec(0.0, Kind::Start),
        rec(0.0, Kind::Up),
        set(0.0, "ups.status", "OL"),
        set(0.0, "battery.charge", "100"),
        set(1000.0, "battery.charge", "90"),
        rec(1060.0, Kind::Tick),
    ];
    let (u, _) = replay(&records, 1100.0);
    assert_eq!(band(&u, 500.0), Band::NoData);
    assert_eq!(u.get("battery.charge"), Some("90"));
    assert!(u.is_live(), "link {:?}", u.link());
    assert_eq!(band(&u, 1000.0), Band::Online);
}

#[test]
fn times_are_whole_seconds() {
    let dir = TempDir::new("times_are_whole_seconds");
    let ops = [
        (T0 + 0.4, Op::Poll(vec![("ups.status".into(), "OL".into())])),
        (T0 + 10.7, Op::Poll(vec![("ups.status".into(), "OB".into())])),
    ];
    let (_, live_events) = live(&ops, true);
    assert_eq!(live_events[0].at, from_unix(T0 + 10.7));
    let (_, events) = replay(&record(&dir.0, "ups@localhost", &ops), 20.0);
    assert_eq!(codes(&events), ["power.lost"]);
    assert_eq!(events[0].at, at(10.0));
}

#[test]
fn two_polls_in_one_second_merge() {
    // An outage shorter than a second cannot be told from the journal.
    let dir = TempDir::new("two_polls_in_one_second");
    let poll = |status: &str| Op::Poll(vec![("ups.status".into(), status.into())]);
    let ops = [(T0, poll("OL")), (T0 + 10.2, poll("OB")), (T0 + 10.8, poll("OL"))];
    assert_eq!(codes(&live(&ops, true).1), ["power.lost", "power.restored"]);
    let (u, events) = replay(&record(&dir.0, "ups@localhost", &ops), 20.0);
    assert!(events.is_empty());
    assert_eq!((u.polls(), u.outages()), (2, 0));
}

// Journal files.

fn journal_file(dir: &Path, lines: &str) -> PathBuf {
    let path = dir.join(journal::file_name("ups@localhost"));
    fs::write(&path, format!("# upst journal {} ups@localhost\n{lines}", journal::VERSION)).unwrap();
    path
}

#[test]
fn a_half_written_line_is_left_out() {
    let dir = TempDir::new("half_written_line");
    let t = T0 as i64;
    let lines = format!("{t} !start\n{t} !up\n{t} ups.status OL\n{t} battery.charge 45\n{} battery.charge 4", t + 60);
    let path = journal_file(&dir.0, &lines);
    let records = journal::read(&path).unwrap();
    assert_eq!(records.last(), Some(&set(0.0, "battery.charge", "45")));
    assert_eq!(records.len(), 4);
    // Reopening picks up the state without it and starts on a fresh line.
    let mut j = Journal::open(&dir.0, "ups@localhost", T0 + 100.0).unwrap();
    assert_eq!(j.state.vars.get("battery.charge").map(String::as_str), Some("45"));
    j.poll(T0 + 100.0, &[("ups.status".into(), "OL".into()), ("battery.charge".into(), "45".into())]).unwrap();
    drop(j);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.ends_with(&format!("{} !start\n{} !up\n", t + 100, t + 100)), "{text}");
}

#[test]
fn a_half_written_line_stays_out_after_reopening() {
    let dir = TempDir::new("half_written_reopened");
    let t = T0 as i64;
    // "45" was cut to "4" by a crash.
    let lines = format!("{t} !start\n{t} !up\n{t} ups.status OL\n{t} battery.charge 45\n{} battery.charge 4", t + 60);
    let path = journal_file(&dir.0, &lines);
    let mut j = Journal::open(&dir.0, "ups@localhost", T0 + 100.0).unwrap();
    let vars = [("ups.status".to_string(), "OL".to_string()), ("battery.charge".into(), "45".into())];
    j.poll(T0 + 100.0, &vars).unwrap();
    let writer = j.state.vars.clone();
    drop(j);
    // What a reader makes of the file is what the writer thinks it says.
    let mut state = journal::State::default();
    journal::read(&path).unwrap().iter().for_each(|r| state.apply(r));
    assert_eq!(state.vars, writer);
    let (u, _) = replay(&journal::read(&path).unwrap(), 110.0);
    assert_eq!(u.get("battery.charge"), Some("45"));
}

#[test]
fn a_clock_that_steps_back_is_clamped() {
    let dir = TempDir::new("clock_steps_back");
    let t = T0 as i64;
    // An hour back after the poll at +60, then on from there.
    let lines = [
        format!("{t} !start"),
        format!("{t} !up"),
        format!("{t} ups.status OL"),
        format!("{t} battery.charge 100"),
        format!("{} battery.charge 99", t + 60),
        format!("{} ups.status OB DISCHRG", t + 60 - 3600),
        format!("{} battery.charge 98", t + 60 - 3540),
        format!("{} battery.charge 97", t + 200),
    ];
    let path = journal_file(&dir.0, &(lines.join("\n") + "\n"));
    let records = journal::read(&path).unwrap();
    let times: Vec<f64> = records.iter().map(|r| r.t - T0).collect();
    assert_eq!(times, [0.0, 0.0, 0.0, 0.0, 60.0, 60.0, 60.0, 200.0]);
    // The polls stuck at +60 are one poll.
    let (u, events) = replay(&records, 300.0);
    assert_eq!(codes(&events), ["power.lost"]);
    assert_eq!(events[0].at, at(60.0));
    assert_eq!((u.get("battery.charge"), u.polls()), (Some("97"), 3));
    // Reopening with the clock still behind keeps the times in order.
    let mut j = Journal::open(&dir.0, "ups@localhost", T0 - 5000.0).unwrap();
    assert_eq!(j.state.last, Some(T0 - 5000.0));
    j.poll(T0 - 4990.0, &[("ups.status".into(), "OL".into())]).unwrap();
    drop(j);
    let records = journal::read(&path).unwrap();
    assert!(records.windows(2).all(|w| w[0].t <= w[1].t));
    assert_eq!(records.last().unwrap().t, T0 + 200.0);
}

#[test]
fn unknown_markers_and_garbage_are_skipped() {
    let dir = TempDir::new("garbage_lines");
    let t = T0 as i64;
    let garbage = [
        format!("{t} !future marker with words"),
        format!("{t} !"),
        "garbage".to_string(),
        String::new(),
        "   ".to_string(),
        "x !up".to_string(),
        "inf !up".to_string(),
        "NaN !down x".to_string(),
        format!("{t}"),
        format!("{t} "),
        "# a comment".to_string(),
    ];
    for g in &garbage {
        assert_eq!(Record::parse(g), None, "{g:?}");
    }
    let mut lines = vec![format!("{t} !start"), format!("{t} !up"), format!("{t} ups.status OL")];
    lines.extend(garbage);
    lines.push(format!("{} battery.charge 50\r", t + 2));
    lines.push(format!("{} !tick", t + 60));
    let path = journal_file(&dir.0, &(lines.join("\n") + "\n"));
    let records = journal::read(&path).unwrap();
    let kinds: Vec<Kind> = records.iter().map(|r| r.kind.clone()).collect();
    assert_eq!(
        kinds,
        [
            Kind::Start,
            Kind::Up,
            Kind::Set("ups.status".into(), "OL".into()),
            Kind::Set("battery.charge".into(), "50".into()),
            Kind::Tick
        ]
    );
    let (u, _) = replay(&records, 100.0);
    assert!(u.is_live());
    assert_eq!((u.get("ups.status"), u.get("battery.charge"), u.polls()), (Some("OL"), Some("50"), 3));
    assert_eq!(journal::read_label(&path).unwrap().as_deref(), Some("ups@localhost"));
    fs::write(&path, "# upst journal 99 ups@localhost\n").unwrap();
    assert_eq!(journal::read_label(&path).unwrap(), None);
}

/// Some text with what needs escaping, spaces, nothing at all, and more
/// than ASCII.
fn text(rng: &mut Rng) -> String {
    const PIECES: &[&str] = &["\\", "\n", "\r", " ", "a", "n", "r", "\\n", "é", "日本", "🔋", "\t", "-", "!", "#", "0"];
    (0..rng.below(7)).map(|_| *rng.pick(PIECES)).collect()
}

/// A variable name: no whitespace, and not starting with `-` or `!`,
/// which NUT names never do.
fn key(rng: &mut Rng) -> String {
    const PIECES: &[&str] = &["battery", ".", "charge", "_", "x", "é", "日", "\\", "9", "n"];
    (0..1 + rng.below(4)).map(|_| *rng.pick(PIECES)).collect()
}

#[test]
fn records_round_trip() {
    let dir = TempDir::new("records_round_trip");
    let mut rng = Rng::new(7);
    let mut t = T0;
    let mut all = Vec::new();
    for _ in 0..1000 {
        t += rng.below(5) as f64;
        let kind = match rng.below(7) {
            0 => Kind::Start,
            1 => Kind::Up,
            2 => Kind::Down(text(&mut rng)),
            3 => Kind::NoData(text(&mut rng)),
            4 => Kind::Tick,
            5 => Kind::Unset(key(&mut rng)),
            _ => Kind::Set(key(&mut rng), text(&mut rng)),
        };
        let r = Record::new(t, kind);
        let line = r.to_string();
        assert!(!line.contains(['\n', '\r']), "{line:?}");
        for l in [line.clone(), format!("{line}\n"), format!("{line}\r\n")] {
            assert_eq!(Record::parse(&l), Some(r.clone()), "{l:?}");
        }
        // Times are whole seconds.
        let late = Record::new(t + 0.75, r.kind.clone());
        assert_eq!(Record::parse(&late.to_string()), Some(r.clone()));
        all.push(r);
    }
    let path = dir.0.join("round.log");
    fs::write(&path, all.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
    assert_eq!(journal::read(&path).unwrap(), all);

    // Through the writer and back.
    for case in 0..200 {
        let label = format!("ups{case}@localhost");
        let mut j = Journal::open(&dir.0, &label, T0).unwrap();
        let mut last = Vec::new();
        for i in 0..5 {
            let vars: BTreeMap<String, String> = (0..rng.below(6)).map(|_| (key(&mut rng), text(&mut rng))).collect();
            last = vars.into_iter().collect::<Vec<_>>();
            j.poll(T0 + i as f64, &last).unwrap();
        }
        let path = j.path.clone();
        drop(j);
        let records = journal::read(&path).unwrap();
        let mut state = journal::State::default();
        records.iter().for_each(|r| state.apply(r));
        assert_eq!(state.vars, last.iter().cloned().collect::<BTreeMap<_, _>>(), "case {case}:{}", dump(&records));
        let (u, _) = replay(&records, 10.0);
        let values: Vec<(String, String)> = u.vars().iter().map(|(k, v)| (k.clone(), v.value.clone())).collect();
        assert_eq!(values, last, "case {case}");
    }
}
