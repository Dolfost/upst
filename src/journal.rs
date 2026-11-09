//! The recorder's journal: one append-only text file per UPS with every
//! variable change and the state of the connection. upst reads the same
//! lines from the recorder's socket and replays them.
//!
//! ```text
//! # upst journal 1 ups@localhost
//! 1762646400 !start
//! 1762646400 !up
//! 1762646400 battery.charge 100
//! 1762646400 ups.status OL
//! 1762646460 !tick
//! 1762646471 battery.charge 99
//! 1762646471 -ups.alarm
//! 1762646500 !down Connection refused
//! ```
//!
//! Times are Unix seconds. A line is a variable and its value (the rest
//! of the line), `-name` for a variable that went away, or a `!` marker.
//! Lines from one poll share a time.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

pub const VERSION: u32 = 1;
/// The recorder writes at least this often while it runs.
pub const TICK: f64 = 60.0;
/// A longer silence means the recorder was not running.
pub const GAP: f64 = TICK * 2.5;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// The recorder started; nothing is known since the previous record.
    Start,
    /// Polls succeed.
    Up,
    /// upsd is unreachable.
    Down(String),
    /// upsd answers, but has no data for this UPS.
    NoData(String),
    /// Still running, nothing changed.
    Tick,
    Set(String, String),
    Unset(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub t: f64,
    pub kind: Kind,
}

impl Record {
    pub fn new(t: f64, kind: Kind) -> Self {
        Record { t, kind }
    }

    pub fn parse(line: &str) -> Option<Record> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with('#') {
            return None;
        }
        let (t, rest) = line.split_once(' ')?;
        let t: f64 = t.parse().ok().filter(|t: &f64| t.is_finite())?;
        let (word, arg) = match rest.split_once(' ') {
            Some((w, a)) => (w, Some(a)),
            None => (rest, None),
        };
        let kind = match word {
            "!start" => Kind::Start,
            "!up" => Kind::Up,
            "!down" => Kind::Down(unescape(arg.unwrap_or(""))),
            "!nodata" => Kind::NoData(unescape(arg.unwrap_or(""))),
            "!tick" => Kind::Tick,
            w if w.starts_with('!') => return None,
            w if w.starts_with('-') && w.len() > 1 => Kind::Unset(w[1..].to_string()),
            "" => return None,
            w => Kind::Set(w.to_string(), unescape(arg.unwrap_or(""))),
        };
        Some(Record { t, kind })
    }
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let t = self.t.floor() as i64;
        match &self.kind {
            Kind::Start => write!(f, "{t} !start"),
            Kind::Up => write!(f, "{t} !up"),
            Kind::Down(why) => write!(f, "{t} !down {}", escape(why)),
            Kind::NoData(why) => write!(f, "{t} !nodata {}", escape(why)),
            Kind::Tick => write!(f, "{t} !tick"),
            Kind::Set(k, v) => write!(f, "{t} {k} {}", escape(v)),
            Kind::Unset(k) => write!(f, "{t} -{k}"),
        }
    }
}

fn escape(s: &str) -> String {
    if !s.contains(['\\', '\n', '\r']) {
        return s.to_string();
    }
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r")
}

fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match (c, c == '\\') {
            (_, true) => match it.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            },
            (c, false) => out.push(c),
        }
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Feed {
    /// After a start, before the first poll.
    #[default]
    Off,
    Up,
    Down(String),
    NoData(String),
}

/// What a journal says at its last record.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub vars: BTreeMap<String, String>,
    pub feed: Feed,
    pub first: Option<f64>,
    pub last: Option<f64>,
}

impl State {
    pub fn apply(&mut self, r: &Record) {
        self.first.get_or_insert(r.t);
        self.last = Some(r.t);
        match &r.kind {
            Kind::Start => self.feed = Feed::Off,
            Kind::Up => self.feed = Feed::Up,
            Kind::Down(why) => self.feed = Feed::Down(why.clone()),
            Kind::NoData(why) => self.feed = Feed::NoData(why.clone()),
            Kind::Tick => {}
            Kind::Set(k, v) => {
                self.vars.insert(k.clone(), v.clone());
            }
            Kind::Unset(k) => {
                self.vars.remove(k);
            }
        }
    }

    /// Records that recreate this state, dated at its last record.
    pub fn snapshot(&self) -> Vec<Record> {
        let Some(t) = self.last else { return Vec::new() };
        let mut out = vec![Record::new(t, Kind::Start)];
        if self.feed == Feed::Up {
            out.push(Record::new(t, Kind::Up));
        }
        out.extend(self.vars.iter().map(|(k, v)| Record::new(t, Kind::Set(k.clone(), v.clone()))));
        match &self.feed {
            Feed::Down(why) => out.push(Record::new(t, Kind::Down(why.clone()))),
            Feed::NoData(why) => out.push(Record::new(t, Kind::NoData(why.clone()))),
            _ => {}
        }
        out
    }
}

/// The records from `since` on, preceded by a snapshot of the state just
/// before it.
pub fn since(records: impl IntoIterator<Item = Record>, since: f64) -> Vec<Record> {
    let mut state = State::default();
    let mut out = Vec::new();
    for r in records {
        if r.t < since {
            state.apply(&r);
            continue;
        }
        if out.is_empty() {
            out.extend(state.snapshot());
        }
        out.push(r);
    }
    if out.is_empty() {
        out = state.snapshot();
    }
    out
}

/// `ups@host:port` as a file name.
pub fn file_name(label: &str) -> String {
    let safe: String =
        label.chars().map(|c| if c.is_ascii_alphanumeric() || "@.:-_[]".contains(c) { c } else { '_' }).collect();
    format!("{safe}.log")
}

fn header(label: &str) -> String {
    format!("# upst journal {VERSION} {label}\n")
}

/// The label in a journal's first line.
pub fn read_label(path: &Path) -> io::Result<Option<String>> {
    let mut first = String::new();
    BufReader::new(File::open(path)?).read_line(&mut first)?;
    let label = first.trim_end().strip_prefix("# upst journal ").and_then(|rest| {
        let (v, label) = rest.split_once(' ')?;
        (v.parse::<u32>().ok()? <= VERSION).then(|| label.to_string())
    });
    Ok(label)
}

/// Every complete record in a journal file, in order. A line still being
/// written (no newline yet) is left out.
pub fn read(path: &Path) -> io::Result<Vec<Record>> {
    let mut out = Vec::new();
    for_each(path, |r| out.push(r))?;
    Ok(out)
}

pub fn for_each(path: &Path, mut f: impl FnMut(Record)) -> io::Result<()> {
    let mut r = BufReader::new(File::open(path)?);
    let mut line = String::new();
    let mut last = f64::NEG_INFINITY;
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 || !line.ends_with('\n') {
            return Ok(());
        }
        if let Some(mut rec) = Record::parse(&line) {
            // A clock that stepped back must not reorder history.
            rec.t = rec.t.max(last);
            last = rec.t;
            f(rec);
        }
    }
}

/// An open journal being appended to.
pub struct Journal {
    pub label: String,
    pub path: PathBuf,
    out: BufWriter<File>,
    /// What the file says so far.
    pub state: State,
}

impl Journal {
    /// Open or create the journal for `label` in `dir`, picking up the
    /// state it ends in, and mark the start.
    pub fn open(dir: &Path, label: &str, t: f64) -> io::Result<Journal> {
        let path = dir.join(file_name(label));
        let mut state = State::default();
        let fresh = match fs::metadata(&path) {
            Ok(m) if m.len() > 0 => {
                for_each(&path, |r| state.apply(&r))?;
                false
            }
            _ => true,
        };
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let mut j = Journal { label: label.to_string(), path, out: BufWriter::new(file), state };
        if fresh {
            j.out.write_all(header(label).as_bytes())?;
        } else if !ends_with_newline(&j.path)? {
            // A crash left half a line behind; start on a fresh one.
            j.out.write_all(b"\n")?;
        }
        j.push(Record::new(t, Kind::Start))?;
        j.out.flush()?;
        Ok(j)
    }

    fn push(&mut self, r: Record) -> io::Result<()> {
        writeln!(self.out, "{r}")?;
        self.state.apply(&r);
        Ok(())
    }

    /// Write what changed since the last poll.
    pub fn poll(&mut self, t: f64, vars: &[(String, String)]) -> io::Result<()> {
        if self.state.feed != Feed::Up {
            self.push(Record::new(t, Kind::Up))?;
        }
        for (k, v) in vars {
            if self.state.vars.get(k) != Some(v) {
                self.push(Record::new(t, Kind::Set(k.clone(), v.clone())))?;
            }
        }
        let gone: Vec<String> =
            self.state.vars.keys().filter(|k| !vars.iter().any(|(n, _)| n == *k)).cloned().collect();
        for k in gone {
            self.push(Record::new(t, Kind::Unset(k)))?;
        }
        self.out.flush()
    }

    pub fn down(&mut self, t: f64, why: &str) -> io::Result<()> {
        if !matches!(self.state.feed, Feed::Down(_)) {
            self.push(Record::new(t, Kind::Down(why.to_string())))?;
        }
        self.out.flush()
    }

    pub fn no_data(&mut self, t: f64, why: &str) -> io::Result<()> {
        if !matches!(self.state.feed, Feed::NoData(_)) {
            self.push(Record::new(t, Kind::NoData(why.to_string())))?;
        }
        self.out.flush()
    }

    /// Mark the journal as alive if it has been quiet for a while.
    pub fn tick(&mut self, t: f64) -> io::Result<()> {
        if self.state.last.is_some_and(|l| t - l >= TICK) {
            self.push(Record::new(t, Kind::Tick))?;
            self.out.flush()?;
        }
        Ok(())
    }

    /// Drop records older than `cutoff` once there are an hour's worth.
    pub fn compact(&mut self, cutoff: f64) -> io::Result<bool> {
        if self.state.first.is_none_or(|f| f >= cutoff - 3600.0) {
            return Ok(false);
        }
        self.out.flush()?;
        let kept = rewrite(&self.path, &self.label, cutoff)?;
        let file = OpenOptions::new().append(true).open(&self.path)?;
        self.out = BufWriter::new(file);
        self.state = kept;
        Ok(true)
    }
}

fn ends_with_newline(path: &Path) -> io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = File::open(path)?;
    if f.seek(SeekFrom::End(0))? == 0 {
        return Ok(true);
    }
    f.seek(SeekFrom::End(-1))?;
    let mut b = [0u8];
    f.read_exact(&mut b)?;
    Ok(b[0] == b'\n')
}

/// Rewrite a journal file without the records before `cutoff`; returns
/// the state it ends in.
pub fn rewrite(path: &Path, label: &str, cutoff: f64) -> io::Result<State> {
    let records = since(read(path)?, cutoff);
    let tmp = path.with_extension("log.tmp");
    let mut state = State::default();
    {
        let mut out = BufWriter::new(File::create(&tmp)?);
        out.write_all(header(label).as_bytes())?;
        for r in &records {
            writeln!(out, "{r}")?;
            state.apply(r);
        }
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(t: f64, kind: Kind) -> Record {
        Record::new(t, kind)
    }

    #[test]
    fn lines_round_trip() {
        let all = [
            rec(10.0, Kind::Start),
            rec(10.0, Kind::Up),
            rec(10.0, Kind::Set("device.model".into(), "EF-UPS-DELTA3 MAX PLUS".into())),
            rec(11.0, Kind::Set("ups.alarm".into(), "".into())),
            rec(12.0, Kind::Set("odd".into(), "a\\b\nc".into())),
            rec(13.0, Kind::Unset("ups.alarm".into())),
            rec(14.0, Kind::Down("Connection refused".into())),
            rec(15.0, Kind::NoData("Driver not connected".into())),
            rec(16.0, Kind::Tick),
        ];
        for r in all {
            let line = format!("{r}\n");
            assert_eq!(Record::parse(&line), Some(r), "{line:?}");
        }
        assert_eq!(Record::parse("# comment"), None);
        assert_eq!(Record::parse("x !up"), None);
        assert_eq!(Record::parse("5 !what"), None);
    }

    #[test]
    fn since_starts_with_a_snapshot() {
        let records = vec![
            rec(0.0, Kind::Start),
            rec(0.0, Kind::Up),
            rec(0.0, Kind::Set("a".into(), "1".into())),
            rec(0.0, Kind::Set("b".into(), "1".into())),
            rec(5.0, Kind::Set("a".into(), "2".into())),
            rec(9.0, Kind::Down("gone".into())),
            rec(20.0, Kind::Up),
            rec(20.0, Kind::Set("b".into(), "3".into())),
        ];
        let got = since(records.clone(), 10.0);
        assert_eq!(
            got,
            vec![
                rec(9.0, Kind::Start),
                rec(9.0, Kind::Set("a".into(), "2".into())),
                rec(9.0, Kind::Set("b".into(), "1".into())),
                rec(9.0, Kind::Down("gone".into())),
                rec(20.0, Kind::Up),
                rec(20.0, Kind::Set("b".into(), "3".into())),
            ]
        );
        assert_eq!(since(records.clone(), 0.0), records);
        assert_eq!(since(records, 99.0).len(), 4);
    }

    #[test]
    fn journal_writes_changes_and_compacts() {
        let dir = std::env::temp_dir().join(format!("upst-journal-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let v = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();

        let mut j = Journal::open(&dir, "ups@localhost", 1000.0).unwrap();
        j.poll(1000.0, &v(&[("a", "1"), ("b", "x")])).unwrap();
        j.poll(1005.0, &v(&[("a", "1"), ("b", "x")])).unwrap();
        j.poll(1010.0, &v(&[("a", "2")])).unwrap();
        j.down(1020.0, "refused").unwrap();
        j.down(1025.0, "refused").unwrap();
        j.tick(1030.0).unwrap();
        j.tick(1090.0).unwrap();
        drop(j);
        let path = dir.join("ups@localhost.log");
        assert_eq!(read_label(&path).unwrap().as_deref(), Some("ups@localhost"));
        let text = fs::read_to_string(&path).unwrap();
        let body: Vec<&str> = text.lines().skip(1).collect();
        assert_eq!(
            body,
            [
                "1000 !start",
                "1000 !up",
                "1000 a 1",
                "1000 b x",
                "1010 a 2",
                "1010 -b",
                "1020 !down refused",
                "1090 !tick"
            ]
        );

        // Reopening picks up the state, so only real changes follow.
        let mut j = Journal::open(&dir, "ups@localhost", 9000.0).unwrap();
        assert_eq!(j.state.vars.get("a").map(String::as_str), Some("2"));
        j.poll(9000.0, &v(&[("a", "2")])).unwrap();
        assert!(j.compact(8000.0).unwrap());
        assert_eq!(j.state.first, Some(1090.0));
        j.poll(9005.0, &v(&[("a", "3")])).unwrap();
        drop(j);
        let got: Vec<String> = read(&path).unwrap().iter().map(|r| r.to_string()).collect();
        assert_eq!(got, ["1090 !start", "1090 a 2", "1090 !down refused", "9000 !start", "9000 !up", "9005 a 3",]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
