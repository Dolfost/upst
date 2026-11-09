//! In-memory time series behind the charts, sparklines and estimates.
//! Times are seconds since the Unix epoch as f64.

use std::collections::{HashMap, VecDeque};

#[derive(Clone, Debug, Default)]
pub struct Series {
    points: VecDeque<(f64, f64)>,
}

impl Series {
    /// Runs of equal values collapse to their two ends, so a value that
    /// sits still for hours costs two points.
    pub fn push(&mut self, t: f64, v: f64) {
        let n = self.points.len();
        if n >= 2 && self.points[n - 1].1 == v && self.points[n - 2].1 == v {
            self.points[n - 1].0 = t;
        } else {
            self.points.push_back((t, v));
        }
    }

    /// Drop points older than `before`, keeping the one in effect at
    /// `before` so step lookups at the window edge still work.
    fn trim(&mut self, before: f64, max: usize) {
        while self.points.len() > 2 && self.points[1].0 <= before {
            self.points.pop_front();
        }
        while self.points.len() > max {
            self.points.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn last(&self) -> Option<(f64, f64)> {
        self.points.back().copied()
    }

    pub fn first(&self) -> Option<(f64, f64)> {
        self.points.front().copied()
    }

    /// Value in effect at `t` (the last point at or before it).
    pub fn at(&self, t: f64) -> Option<f64> {
        let i = self.points.partition_point(|p| p.0 <= t);
        (i > 0).then(|| self.points[i - 1].1)
    }

    /// Points inside `[t0, t1]`, preceded by the value in effect at `t0`.
    pub fn window(&self, t0: f64, t1: f64) -> impl Iterator<Item = (f64, f64)> + '_ {
        let start = self.points.partition_point(|p| p.0 < t0);
        let lead = (start > 0).then(|| (t0, self.points[start - 1].1));
        lead.into_iter().chain(self.points.range(start..).copied().take_while(move |p| p.0 <= t1))
    }

    pub fn min_max(&self, t0: f64, t1: f64) -> Option<(f64, f64)> {
        self.window(t0, t1).fold(None, |acc, (_, v)| match acc {
            None => Some((v, v)),
            Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
        })
    }

    /// True once the series held more than one value.
    pub fn varied(&self) -> bool {
        let mut it = self.points.iter();
        match it.next() {
            Some(&(_, first)) => it.any(|p| p.1 != first),
            None => false,
        }
    }

    /// At most two points (the low and the high, in time order) per
    /// bucket, so a chart keeps its spikes without plotting thousands.
    pub fn chart_points(&self, t0: f64, t1: f64, buckets: usize) -> Vec<(f64, f64)> {
        let buckets = buckets.max(1);
        let width = (t1 - t0).max(1e-9);
        let mut out = Vec::with_capacity(buckets * 2 + 2);
        // (bucket, lowest point, highest point)
        type Bucket = (usize, (f64, f64), (f64, f64));
        let mut cur: Option<Bucket> = None;
        let flush = |out: &mut Vec<(f64, f64)>, lo: (f64, f64), hi: (f64, f64)| {
            if lo.0 == hi.0 {
                out.push(lo);
            } else if lo.0 < hi.0 {
                out.push(lo);
                out.push(hi);
            } else {
                out.push(hi);
                out.push(lo);
            }
        };
        for p in self.window(t0, t1) {
            let b = (((p.0 - t0) / width) * buckets as f64) as usize;
            match &mut cur {
                Some((cb, lo, hi)) if *cb == b => {
                    if p.1 < lo.1 {
                        *lo = p;
                    }
                    if p.1 >= hi.1 {
                        *hi = p;
                    }
                }
                _ => {
                    if let Some((_, lo, hi)) = cur.take() {
                        flush(&mut out, lo, hi);
                    }
                    cur = Some((b, p, p));
                }
            }
        }
        if let Some((_, lo, hi)) = cur {
            flush(&mut out, lo, hi);
        }
        out
    }

    /// One value per column for sparklines: the mean of the points in
    /// the column, else the value carried over from before it. `None`
    /// before the first point and after the last one.
    pub fn columns(&self, t0: f64, t1: f64, n: usize) -> Vec<Option<f64>> {
        let (Some(first), Some(last)) = (self.first(), self.last()) else {
            return vec![None; n];
        };
        let w = (t1 - t0) / n.max(1) as f64;
        let mut out = Vec::with_capacity(n);
        let mut i = self.points.partition_point(|p| p.0 < t0);
        for c in 0..n {
            let (a, b) = (t0 + w * c as f64, t0 + w * (c + 1) as f64);
            let (mut sum, mut cnt) = (0.0, 0usize);
            while i < self.points.len() && self.points[i].0 < b {
                sum += self.points[i].1;
                cnt += 1;
                i += 1;
            }
            out.push(if cnt > 0 {
                Some(sum / cnt as f64)
            } else if b < first.0 || a > last.0 {
                None
            } else {
                self.at(a)
            });
        }
        out
    }
}

/// What a moment of the status timeline looked like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Online,
    Battery,
    Low,
    Other,
    NoData,
}

pub struct History {
    series: HashMap<String, Series>,
    bands: VecDeque<(f64, Band)>,
    keep: f64,
    max_points: usize,
}

impl History {
    pub fn new(keep_secs: f64, interval_secs: f64) -> Self {
        // Enough points for the whole window at the poll rate, with a
        // ceiling so a tiny interval cannot eat the memory.
        let max_points = ((keep_secs / interval_secs.max(0.1)) as usize + 16).clamp(256, 200_000);
        History { series: HashMap::new(), bands: VecDeque::new(), keep: keep_secs, max_points }
    }

    pub fn record(&mut self, t: f64, name: &str, v: f64) {
        match self.series.get_mut(name) {
            Some(s) => s.push(t, v),
            None => {
                let mut s = Series::default();
                s.push(t, v);
                self.series.insert(name.to_string(), s);
            }
        }
    }

    pub fn band(&mut self, t: f64, band: Band) {
        if self.bands.back().is_none_or(|b| b.1 != band) {
            self.bands.push_back((t, band));
        }
    }

    pub fn trim(&mut self, now: f64) {
        let before = now - self.keep;
        for s in self.series.values_mut() {
            s.trim(before, self.max_points);
        }
        while self.bands.len() > 1 && self.bands[1].0 <= before {
            self.bands.pop_front();
        }
    }

    pub fn get(&self, name: &str) -> Option<&Series> {
        self.series.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.series.keys().map(String::as_str)
    }

    /// Band per column across `[t0, t1]`.
    pub fn band_columns(&self, t0: f64, t1: f64, n: usize, now: f64) -> Vec<Band> {
        let w = (t1 - t0) / n.max(1) as f64;
        (0..n)
            .map(|c| {
                let mid = t0 + w * (c as f64 + 0.5);
                if mid > now {
                    return Band::NoData;
                }
                let i = self.bands.partition_point(|b| b.0 <= mid);
                if i == 0 { Band::NoData } else { self.bands[i - 1].1 }
            })
            .collect()
    }
}

/// Which way the charge is heading in the current segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trend {
    Discharging,
    Charging,
}

/// Follows battery.charge through one charge or discharge segment and
/// turns its steps into a rate. Charge is usually reported in whole
/// percent, so the rate is measured between step edges rather than fitted
/// over the flat stretches.
#[derive(Debug, Default)]
pub struct Tracker {
    trend: Option<Trend>,
    start: (f64, f64),
    steps: Vec<(f64, f64)>,
}

impl Tracker {
    pub fn observe(&mut self, t: f64, charge: f64, trend: Option<Trend>) {
        if trend != self.trend {
            self.trend = trend;
            self.start = (t, charge);
            self.steps.clear();
            return;
        }
        let last = self.steps.last().map_or(self.start.1, |s| s.1);
        if trend.is_some() && charge != last {
            self.steps.push((t, charge));
            if self.steps.len() > 64 {
                self.steps.drain(..32);
            }
        }
    }

    pub fn reset(&mut self) {
        *self = Tracker::default();
    }

    pub fn trend(&self) -> Option<Trend> {
        self.trend
    }

    /// Percent per second (negative while discharging) and whether it
    /// rests on a single step.
    pub fn rate(&self, now: f64) -> Option<(f64, bool)> {
        let trend = self.trend?;
        let n = self.steps.len();
        let (rate, rough) = match n {
            0 => return None,
            1 => {
                let (t0, v0) = self.start;
                let (_, v1) = self.steps[0];
                ((v1 - v0) / (now - t0).max(1.0), true)
            }
            _ => {
                let a = self.steps[n.saturating_sub(12)];
                let b = self.steps[n - 1];
                if b.0 - a.0 < 1.0 {
                    return None;
                }
                let mut rate = (b.1 - a.1) / (b.0 - a.0);
                // No step for much longer than the usual step time means
                // the rate dropped; fall back to the average up to now.
                let per_step = (b.0 - a.0) / (self.steps.len().min(12) - 1) as f64;
                if now - b.0 > per_step * 1.5 {
                    rate = (b.1 - a.1) / (now - a.0);
                }
                (rate, false)
            }
        };
        let ok = match trend {
            Trend::Discharging => rate < 0.0,
            Trend::Charging => rate > 0.0,
        };
        ok.then_some((rate, rough))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_flat_runs() {
        let mut s = Series::default();
        for t in 0..100 {
            s.push(t as f64, 5.0);
        }
        assert_eq!(s.len(), 2);
        s.push(100.0, 6.0);
        assert_eq!(s.len(), 3);
        assert_eq!(s.at(50.0), Some(5.0));
        assert_eq!(s.at(100.0), Some(6.0));
        assert_eq!(s.at(-1.0), None);
    }

    #[test]
    fn window_carries_value_in() {
        let mut s = Series::default();
        s.push(0.0, 1.0);
        s.push(10.0, 2.0);
        s.push(20.0, 3.0);
        let w: Vec<_> = s.window(5.0, 15.0).collect();
        assert_eq!(w, [(5.0, 1.0), (10.0, 2.0)]);
        assert_eq!(s.min_max(5.0, 25.0), Some((1.0, 3.0)));
    }

    #[test]
    fn columns_fill_gaps() {
        let mut s = Series::default();
        s.push(0.0, 1.0);
        s.push(1.0, 2.0);
        s.push(9.0, 4.0);
        let c = s.columns(-10.0, 10.0, 4);
        assert_eq!(c, [None, None, Some(1.5), Some(4.0)]);
    }

    #[test]
    fn tracker_rates() {
        let mut tr = Tracker::default();
        let d = Some(Trend::Discharging);
        tr.observe(0.0, 100.0, d);
        assert!(tr.rate(10.0).is_none());
        // 1 % every 60 s.
        for i in 1..=5 {
            tr.observe(i as f64 * 60.0, 100.0 - i as f64, d);
        }
        let (r, rough) = tr.rate(300.0).unwrap();
        assert!(!rough);
        assert!((r + 1.0 / 60.0).abs() < 1e-9);
        // Charging after that starts over.
        tr.observe(400.0, 95.0, Some(Trend::Charging));
        assert!(tr.rate(400.0).is_none());
    }
}
