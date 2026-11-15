//! Golden tests of the UPS model against the behaviour of upst 0.1.2.
//!
//! `golden/scenarios/*.scn` script polls, link losses, gaps, estimates and
//! journal replays; `golden/expected/*.golden` is what the model of upst
//! 0.1.2 made of them, generated before the model moved into this crate.
//! The output has to stay the same, byte for byte, apart from deliberate
//! changes listed in `golden/README.md`. `golden/README.md` also has the
//! file format.
//!
//! To look at a difference, or to accept a deliberate one:
//!
//! ```text
//! UPST_GOLDEN_OUT=/tmp/golden cargo test -p upst-core --test golden   # writes what it got
//! UPST_GOLDEN_BLESS=1 cargo test -p upst-core --test golden          # overwrites expected/
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use upst_core::history::History;
use upst_core::journal::Record;
use upst_core::{Estimate, Event, EventKind, Link, Ups, from_unix, unix};

use scn::{Scenario, Step};

pub mod scn {
    //! Scenario file format, see the README next to the scenarios.

    #[derive(Clone, Debug, PartialEq)]
    pub enum Step {
        /// The full variable list of one poll (`poll+` is expanded here).
        Poll {
            t: f64,
            vars: Vec<(String, String)>,
        },
        Down {
            t: f64,
            why: String,
        },
        Stale {
            t: f64,
            why: String,
        },
        Connecting {
            t: f64,
        },
        Gap {
            t: f64,
        },
        Estimate {
            t: f64,
        },
        State {
            t: f64,
        },
        /// Raw journal lines (journal.rs format) replayed up to `until`.
        Replay {
            lines: Vec<String>,
            until: f64,
        },
    }

    impl Step {
        /// Every time this step mentions, for the scenario's time range.
        pub fn times(&self) -> Vec<f64> {
            match self {
                Step::Poll { t, .. }
                | Step::Down { t, .. }
                | Step::Stale { t, .. }
                | Step::Connecting { t }
                | Step::Gap { t }
                | Step::Estimate { t }
                | Step::State { t } => vec![*t],
                Step::Replay { lines, until } => {
                    let mut v: Vec<f64> = lines
                        .iter()
                        .filter_map(|l| l.split(' ').next()?.parse::<f64>().ok())
                        .filter(|t| t.is_finite())
                        .collect();
                    v.push(*until);
                    v
                }
            }
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Scenario {
        pub name: String,
        pub capacity: Option<f64>,
        pub steps: Vec<Step>,
    }

    impl Scenario {
        /// Earliest and latest time any step mentions.
        pub fn range(&self) -> Option<(f64, f64)> {
            let all: Vec<f64> = self.steps.iter().flat_map(Step::times).collect();
            let lo = all.iter().copied().reduce(f64::min)?;
            let hi = all.iter().copied().reduce(f64::max)?;
            Some((lo, hi))
        }
    }

    enum Block {
        None,
        Poll { t: f64, vars: Vec<(String, String)>, delta: bool },
        Journal { lines: Vec<String> },
    }

    /// Drop a ` #comment` from a directive line.
    fn directive(line: &str) -> &str {
        let cut =
            line.char_indices().find(|&(i, c)| c == '#' && i > 0 && line[..i].ends_with(|p: char| p.is_whitespace()));
        match cut {
            Some((i, _)) => line[..i].trim_end(),
            None => line.trim_end(),
        }
    }

    fn time(s: &str, n: usize) -> Result<f64, String> {
        s.parse::<f64>().ok().filter(|t| t.is_finite()).ok_or_else(|| format!("line {n}: bad time {s:?}"))
    }

    pub fn parse(text: &str) -> Result<Vec<Scenario>, String> {
        let mut out: Vec<Scenario> = Vec::new();
        let mut block = Block::None;
        // The expanded list of the last poll, the base of `poll+`.
        let mut last_poll: Vec<(String, String)> = Vec::new();

        fn close(
            block: &mut Block,
            out: &mut [Scenario],
            last_poll: &mut Vec<(String, String)>,
            n: usize,
        ) -> Result<(), String> {
            match std::mem::replace(block, Block::None) {
                Block::None => {}
                Block::Poll { t, vars, .. } => {
                    *last_poll = vars.clone();
                    out.last_mut()
                        .ok_or(format!("line {n}: step outside a scenario"))?
                        .steps
                        .push(Step::Poll { t, vars });
                }
                Block::Journal { .. } => return Err(format!("line {n}: replay block without replay-until")),
            }
            Ok(())
        }

        for (i, raw) in text.lines().enumerate() {
            let n = i + 1;
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            if let Block::Journal { lines } = &mut block {
                if let Some(rest) = line.strip_prefix("replay-until") {
                    let until = time(directive(rest).trim(), n)?;
                    let lines = std::mem::take(lines);
                    block = Block::None;
                    out.last_mut()
                        .ok_or(format!("line {n}: step outside a scenario"))?
                        .steps
                        .push(Step::Replay { lines, until });
                } else if line.starts_with('@') || line.starts_with("===") {
                    return Err(format!("line {n}: replay block without replay-until"));
                } else {
                    lines.push(line.to_string());
                }
                continue;
            }

            if let Some(name) = line.strip_prefix("===") {
                close(&mut block, &mut out, &mut last_poll, n)?;
                last_poll.clear();
                let name = directive(name).trim();
                if name.is_empty() {
                    return Err(format!("line {n}: scenario without a name"));
                }
                if out.iter().any(|s| s.name == name) {
                    return Err(format!("line {n}: duplicate scenario {name:?}"));
                }
                out.push(Scenario { name: name.to_string(), capacity: None, steps: Vec::new() });
                continue;
            }

            if let Some(rest) = line.strip_prefix('@') {
                close(&mut block, &mut out, &mut last_poll, n)?;
                let rest = directive(rest);
                let (t, rest) = rest.split_once(' ').ok_or(format!("line {n}: @<time> needs a command"))?;
                let t = time(t, n)?;
                let (cmd, arg) = rest.split_once(' ').unwrap_or((rest, ""));
                let step = match cmd {
                    "poll" => {
                        block = Block::Poll { t, vars: Vec::new(), delta: false };
                        None
                    }
                    "poll+" => {
                        block = Block::Poll { t, vars: last_poll.clone(), delta: true };
                        None
                    }
                    "down" => Some(Step::Down { t, why: arg.to_string() }),
                    "stale" => Some(Step::Stale { t, why: arg.to_string() }),
                    "connecting" => Some(Step::Connecting { t }),
                    "gap" => Some(Step::Gap { t }),
                    "estimate" => Some(Step::Estimate { t }),
                    "state" => Some(Step::State { t }),
                    other => return Err(format!("line {n}: unknown command {other:?}")),
                };
                if let Some(step) = step {
                    out.last_mut().ok_or(format!("line {n}: step outside a scenario"))?.steps.push(step);
                }
                continue;
            }

            if directive(line) == "replay" {
                close(&mut block, &mut out, &mut last_poll, n)?;
                if out.is_empty() {
                    return Err(format!("line {n}: step outside a scenario"));
                }
                block = Block::Journal { lines: Vec::new() };
                continue;
            }

            match &mut block {
                Block::Poll { vars, delta, .. } => {
                    let line = line.trim_start();
                    if let Some(name) = line.strip_prefix('-') {
                        if !*delta {
                            return Err(format!("line {n}: -name only works in poll+"));
                        }
                        let before = vars.len();
                        vars.retain(|(k, _)| k != name);
                        if vars.len() == before {
                            return Err(format!("line {n}: -{name} is not in the previous poll"));
                        }
                    } else {
                        let (k, v) = line.split_once(' ').unwrap_or((line, ""));
                        // `"..."` keeps blanks an editor would strip.
                        let v = match v.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                            Some(inner) => inner,
                            None => v,
                        };
                        if *delta && let Some(slot) = vars.iter_mut().find(|(name, _)| name == k) {
                            slot.1 = v.to_string();
                        } else if !*delta && vars.iter().any(|(name, _)| name == k) {
                            return Err(format!("line {n}: {k} listed twice"));
                        } else {
                            vars.push((k.to_string(), v.to_string()));
                        }
                    }
                }
                Block::None => {
                    let d = directive(line);
                    if let Some(c) = d.strip_prefix("capacity ") {
                        let s = out.last_mut().ok_or(format!("line {n}: capacity outside a scenario"))?;
                        if !s.steps.is_empty() {
                            return Err(format!("line {n}: capacity after the first step"));
                        }
                        s.capacity = Some(c.trim().parse().map_err(|_| format!("line {n}: bad capacity {c:?}"))?);
                    } else {
                        return Err(format!("line {n}: unexpected line {line:?}"));
                    }
                }
                Block::Journal { .. } => unreachable!(),
            }
        }
        let n = text.lines().count();
        close(&mut block, &mut out, &mut last_poll, n)?;
        Ok(out)
    }
}

// Runner

/// Columns of `band_columns` in the state summary.
const BAND_COLUMNS: usize = 20;

fn ft(t: f64) -> String {
    if t.fract() == 0.0 && t.abs() < 1e15 { format!("{}", t as i64) } else { format!("{t:.3}") }
}

fn fo3(v: Option<f64>) -> String {
    v.map_or("None".into(), |x| format!("{x:.3}"))
}

fn fsys(t: Option<SystemTime>) -> String {
    t.map_or("None".into(), |t| ft(unix(t)))
}

fn f1(t: Option<SystemTime>) -> String {
    t.map_or("None".into(), |t| format!("{:.1}", unix(t)))
}

fn estimate_line(e: Option<Estimate>) -> String {
    match e {
        None => "estimate None".into(),
        Some(e) => format!(
            "estimate trend={:?} per_hour={:.6} rough={} to_low={} to_empty={} to_full={} watts={}",
            e.trend,
            e.per_hour,
            e.rough,
            fo3(e.to_low),
            fo3(e.to_empty),
            fo3(e.to_full),
            fo3(e.watts)
        ),
    }
}

fn state(out: &mut String, u: &Ups, t0: f64, t1: f64) {
    let power_since = u.power_since().map_or("None".into(), |(t, seen)| format!("({}, {seen})", ft(unix(t))));
    let (headline, _) = u.status().headline();
    let _ = writeln!(
        out,
        "state link={:?} outages={} battery_secs={:.3} outage_start={} power_since={} since={} polls={} headline={:?} power={:?} capacity_wh={:?} capacity_guessed={}",
        u.link(),
        u.outages(),
        u.battery_secs(),
        fsys(u.outage_start()),
        power_since,
        fsys(u.since()),
        u.polls(),
        headline,
        u.status().power(),
        u.capacity_wh(),
        u.capacity_guessed(),
    );
    let _ = writeln!(
        out,
        "status tokens={:?} severity={:?} trend={:?} updated={}",
        u.status().tokens(),
        u.status().severity(),
        u.tracker().trend(),
        f1(u.updated()),
    );
    for (k, v) in u.vars() {
        let _ = writeln!(out, "var {k} {:?}", v.value);
    }
    for (k, v) in u.vars() {
        if v.changed.is_some() {
            let _ = writeln!(out, "changed {k} prev={:?} at={}", v.prev, f1(v.changed));
        }
    }
    let history = u.history().expect("the runner builds every Ups with a history");
    let bands = history.band_columns(t0, t1, BAND_COLUMNS, t1);
    let bands: Vec<String> = bands.iter().map(|b| format!("{b:?}")).collect();
    let _ = writeln!(out, "bands {}..{} {}", ft(t0), ft(t1), bands.join(","));
    let mut names: Vec<&str> = history.names().collect();
    names.sort_unstable();
    for name in names {
        let s = history.get(name).unwrap();
        let pts: Vec<String> =
            s.window(f64::NEG_INFINITY, f64::INFINITY).map(|(t, v)| format!("{}:{v}", ft(t))).collect();
        let _ = writeln!(out, "series {name} {}", pts.join(" "));
    }
    if let Some(s) = u.series("battery.charge") {
        let at: Vec<String> = (0..=4)
            .map(|k| {
                let t = t0 + (t1 - t0) * k as f64 / 4.0;
                format!("{}={}", ft(t), s.at(t).map_or("None".into(), |v| v.to_string()))
            })
            .collect();
        let _ = writeln!(out, "charge_at {}", at.join(" "));
    }
    let _ = writeln!(out, "now={} {}", ft(t1), estimate_line(u.estimate(from_unix(t1))));
}

fn events(out: &mut String, evs: impl IntoIterator<Item = Event>) {
    for ev in evs {
        let _ = writeln!(out, "  {} {:?} {ev}", ft(unix(ev.at)), ev.severity);
    }
}

pub fn run(s: &Scenario) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "=== {}", s.name);
    if let Some(c) = s.capacity {
        let _ = writeln!(out, "capacity {c}");
    }
    let mut u =
        Ups::builder("ups", "localhost").capacity_wh(s.capacity).history(Some(History::new(86400.0, 2.0))).build();
    let (t0, t1) = s.range().unwrap_or((0.0, 0.0));
    for step in &s.steps {
        match step {
            Step::Poll { t, vars } => {
                let _ = writeln!(out, "@{} poll {} vars", ft(*t), vars.len());
                // Live, upst logs its own server-wide "Reconnected to ..." and
                // leaves this one out, as upst 0.1.2 had no such event live.
                let evs = u.apply(vars.clone(), from_unix(*t));
                events(&mut out, evs.into_iter().filter(|e| !matches!(e.kind, EventKind::UpsdBack { .. })));
                assert_eq!(u.updated(), Some(from_unix(*t)));
            }
            Step::Down { t, why } => {
                let _ = writeln!(out, "@{} down {why}", ft(*t));
                events(&mut out, u.lost(Link::Down(why.clone()), from_unix(*t)));
            }
            Step::Stale { t, why } => {
                let _ = writeln!(out, "@{} stale {why}", ft(*t));
                events(&mut out, u.lost(Link::Stale(why.clone()), from_unix(*t)));
            }
            Step::Connecting { t } => {
                let _ = writeln!(out, "@{} connecting", ft(*t));
                events(&mut out, u.lost(Link::Connecting, from_unix(*t)));
            }
            Step::Gap { t } => {
                let _ = writeln!(out, "@{} gap", ft(*t));
                u.gap(from_unix(*t));
            }
            Step::Estimate { t } => {
                let _ = writeln!(out, "@{} estimate", ft(*t));
                let _ = writeln!(out, "  {}", estimate_line(u.estimate(from_unix(*t))));
            }
            Step::State { t } => {
                let _ = writeln!(out, "@{} state", ft(*t));
                state(&mut out, &u, t0, *t);
            }
            Step::Replay { lines, until } => {
                let records: Vec<Record> = lines
                    .iter()
                    .map(|l| Record::parse(l).unwrap_or_else(|| panic!("{}: bad journal line {l:?}", s.name)))
                    .collect();
                let _ = writeln!(out, "@{} replay-until {} records", ft(*until), records.len());
                events(&mut out, u.replay(&records, from_unix(*until)));
            }
        }
    }
    let _ = writeln!(out, "--- end");
    state(&mut out, &u, t0, t1);
    out
}

/// All scenarios of one file, separated by blank lines.
pub fn run_file(text: &str) -> Result<String, String> {
    let scenarios = scn::parse(text)?;
    Ok(scenarios.iter().map(run).collect::<Vec<_>>().join("\n"))
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

#[test]
fn matches_upst_0_1_2() {
    let out = std::env::var_os("UPST_GOLDEN_OUT").map(PathBuf::from);
    let bless = std::env::var_os("UPST_GOLDEN_BLESS").is_some();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir().join("scenarios"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "scn"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no scenarios");
    let mut failures = Vec::new();
    let mut count = 0;
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        count += scn::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", f.display())).len();
        let got = run_file(&text).unwrap();
        let name = format!("{}.golden", f.file_stem().unwrap().to_string_lossy());
        if let Some(dir) = &out {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(&name), &got).unwrap();
        }
        let expected = dir().join("expected").join(&name);
        if bless {
            std::fs::write(&expected, &got).unwrap();
            continue;
        }
        let want = std::fs::read_to_string(&expected).unwrap_or_default();
        if want != got {
            let line = want
                .lines()
                .zip(got.lines())
                .position(|(a, b)| a != b)
                .unwrap_or(want.lines().count().min(got.lines().count()));
            failures.push(format!(
                "{name} differs at line {}:\n  want: {}\n  got:  {}",
                line + 1,
                want.lines().nth(line).unwrap_or("<eof>"),
                got.lines().nth(line).unwrap_or("<eof>")
            ));
        }
    }
    assert!(count > 80, "only {count} scenarios");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
