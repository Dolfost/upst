//! The dashboard: a set of cards flowed into one to three columns. Each
//! card knows its natural height for a given width and can use spare
//! rows (big digits, taller sparklines, more events); cards that do not
//! fit are dropped from the end of the priority list.

use std::time::SystemTime;

use chrono::Local;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::charge::{self, Charge};
use super::widgets::{bar, kv, meter, spark_range, sparkline};
use super::{Ctx, Theme, ups_severity};
use crate::app::{ServerLink, WINDOWS};
use crate::fmt;
use crate::history::Trend;
use crate::model::{Link, Ups, unix};
use crate::status::{Severity, flag_info};

const LW: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Card {
    Status,
    Battery,
    Power,
    Line,
    Environment,
    Trends,
    Device,
    Events,
}

impl Card {
    fn title(self) -> &'static str {
        match self {
            Card::Status => "Status",
            Card::Battery => "Battery",
            Card::Power => "Load",
            Card::Line => "Line",
            Card::Environment => "Environment",
            Card::Trends => "Trends",
            Card::Device => "Device",
            Card::Events => "Recent events",
        }
    }

    /// Content lines a card keeps when space is short.
    fn min_lines(self) -> u16 {
        match self {
            Card::Environment => 1,
            Card::Battery => 3,
            _ => 2,
        }
    }

    /// Share of a column's spare rows.
    fn weight(self) -> u16 {
        match self {
            Card::Events => 3,
            Card::Trends => 2,
            Card::Battery => 2,
            _ => 0,
        }
    }
}

fn cards(u: &Ups) -> Vec<Card> {
    let mut v = vec![Card::Status];
    if u.has_prefix("battery.") {
        v.push(Card::Battery);
    }
    if u.has_any(&["ups.load", "ups.realpower", "ups.power", "output.current", "ups.efficiency"])
        || !outlets(u).is_empty()
    {
        v.push(Card::Power);
    }
    if u.has_any(&["input.voltage", "output.voltage", "input.frequency", "output.frequency"]) {
        v.push(Card::Line);
    }
    if u.has_any(&["ambient.temperature", "ambient.humidity", "ups.temperature"]) {
        v.push(Card::Environment);
    }
    if !trend_metrics(u).is_empty() {
        v.push(Card::Trends);
    }
    if u.has_prefix("device.") || u.has_prefix("driver.") || u.has_any(&["ups.model", "ups.mfr"]) {
        v.push(Card::Device);
    }
    v.push(Card::Events);
    v
}

pub fn draw(f: &mut Frame, area: Rect, cx: &mut Ctx, u: &Ups) {
    let list = cards(u);
    let cols = if area.width >= 132 {
        3
    } else if area.width >= 80 {
        2
    } else {
        1
    };
    let mut cols = cols.min(list.len());
    loop {
        let col_w = area.width / cols as u16;
        let inner_w = col_w.saturating_sub(2) as usize;
        let natural: Vec<u16> = list.iter().map(|c| content(*c, cx, u, inner_w, 0).len() as u16 + 2).collect();
        let small: Vec<u16> = list.iter().zip(&natural).map(|(c, h)| (*h).min(c.min_lines() + 2)).collect();
        // Natural heights if everything fits; otherwise shrunk cards when
        // that shows more of them.
        let (mut n, mut packed, mut base) = (0, None, &natural);
        for heights in [&natural, &small] {
            let (k, p) = fit(heights, cols, area.height);
            if k > n {
                (n, packed, base) = (k, p, heights);
            }
            if n == list.len() {
                break;
            }
        }
        // A pane this small reads better as a plain summary.
        let Some(columns) = packed.filter(|_| n >= list.len().min(3) || area.height >= 16) else {
            compact(f, area, cx, u);
            return;
        };
        if columns.len() < cols {
            // Fewer columns were enough: redo them wider.
            cols = columns.len();
            continue;
        }
        for (ci, column) in columns.iter().enumerate() {
            let x = area.x + col_w * ci as u16;
            let w = if ci == cols - 1 { area.width - col_w * ci as u16 } else { col_w };
            let mut heights: Vec<u16> = column.iter().map(|&i| base[i]).collect();
            let mut spare = area.height.saturating_sub(heights.iter().sum());
            // Shrunk cards grow back first, top to bottom.
            for (k, &i) in column.iter().enumerate() {
                let grow = natural[i].saturating_sub(heights[k]).min(spare);
                heights[k] += grow;
                spare -= grow;
            }
            let extra = share(&column.iter().map(|&i| list[i].weight()).collect::<Vec<_>>(), spare);
            let mut y = area.y;
            for (k, &i) in column.iter().enumerate() {
                let h = heights[k] + extra[k];
                render(f, Rect::new(x, y, w, h), cx, u, list[i], h.saturating_sub(natural[i]));
                y += h;
            }
        }
        if n < list.len() {
            let hidden: Vec<&str> = list[n..].iter().map(|c| c.title()).collect();
            let note = format!(" +{} hidden: {} ", hidden.len(), hidden.join(", "));
            let r = Rect::new(area.x, area.y + area.height - 1, area.width.saturating_sub(2), 1);
            f.render_widget(Paragraph::new(Span::styled(note, cx.t.dim())).alignment(Alignment::Right), r);
        }
        return;
    }
}

/// How many leading cards fit into the columns, and where they go.
fn fit(heights: &[u16], cols: usize, avail: u16) -> (usize, Option<Vec<Vec<usize>>>) {
    (1..=heights.len()).rev().find_map(|n| pack(&heights[..n], cols, avail).map(|p| (n, Some(p)))).unwrap_or((0, None))
}

/// Smallest column height that fits all cards, in order, into `cols`.
fn pack(heights: &[u16], cols: usize, avail: u16) -> Option<Vec<Vec<usize>>> {
    let tallest = *heights.iter().max()?;
    if tallest > avail {
        return None;
    }
    (tallest..=avail).find_map(|cap| {
        let mut columns = vec![Vec::new()];
        let mut used = 0;
        for (i, &h) in heights.iter().enumerate() {
            if used + h > cap {
                columns.push(Vec::new());
                used = 0;
            }
            if columns.len() > cols {
                return None;
            }
            columns.last_mut().unwrap().push(i);
            used += h;
        }
        Some(columns)
    })
}

/// Split `spare` rows by weight; with no weights the last card takes all.
fn share(weights: &[u16], spare: u16) -> Vec<u16> {
    let mut out = vec![0; weights.len()];
    let total: u16 = weights.iter().sum();
    if weights.is_empty() {
        return out;
    }
    if total == 0 {
        *out.last_mut().unwrap() = spare;
        return out;
    }
    let mut given = 0;
    for (i, w) in weights.iter().enumerate() {
        out[i] = spare * w / total;
        given += out[i];
    }
    let last = weights.iter().rposition(|w| *w > 0).unwrap();
    out[last] += spare - given;
    out
}

fn render(f: &mut Frame, rect: Rect, cx: &Ctx, u: &Ups, card: Card, extra: u16) {
    let t = cx.t;
    let mut block = t.block(false).title(format!(" {} ", card.title()));
    let title_right = match card {
        Card::Trends => Some(fmt::span(WINDOWS[cx.app.hist.window])),
        Card::Status => u.updated.map(|at| {
            let every = if cx.app.paused {
                "paused".to_string()
            } else {
                format!("every {}", fmt::duration(cx.app.interval.as_secs_f64()))
            };
            format!("{}, {every}", fmt::ago(at.elapsed()))
        }),
        _ => None,
    };
    if let Some(r) = title_right {
        block = block.title(Line::from(Span::styled(format!(" {r} "), t.dim())).right_aligned());
    }
    if card == Card::Status {
        block = block.border_style(t.sev(ups_severity(u)));
    }
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let lines = content(card, cx, u, inner.width as usize, extra);
    f.render_widget(Paragraph::new(lines), inner);
}

fn content(card: Card, cx: &Ctx, u: &Ups, w: usize, extra: u16) -> Vec<Line<'static>> {
    match card {
        Card::Status => status(cx, u, w),
        Card::Battery => battery(cx, u, w, extra),
        Card::Power => power(&cx.t, u, w),
        Card::Line => line(&cx.t, u, w),
        Card::Environment => environment(&cx.t, u, w),
        Card::Trends => trends(cx, u, w, extra),
        Card::Device => device(cx, u, w),
        Card::Events => events(cx, u, w, extra),
    }
}

/// Greedy word wrap for plain text.
pub fn wrap(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let width = width.max(8);
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let need = if cur.is_empty() { word.chars().count() } else { cur.chars().count() + 1 + word.chars().count() };
        if need > width && !cur.is_empty() {
            lines.push(Line::from(Span::styled(std::mem::take(&mut cur), style)));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(Line::from(Span::styled(cur, style)));
    }
    lines
}

fn dim(t: &Theme, s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), t.dim())
}

fn val(t: &Theme, s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), t.value())
}

/// `08:42`, or with the day when it is not today.
fn clock_in(secs: f64) -> String {
    let at = Local::now() + chrono::Duration::milliseconds((secs * 1000.0) as i64);
    if at.date_naive() == Local::now().date_naive() {
        at.format("%H:%M").to_string()
    } else {
        at.format("%a %H:%M").to_string()
    }
}

fn status(cx: &Ctx, u: &Ups, w: usize) -> Vec<Line<'static>> {
    let t = &cx.t;
    let app = cx.app;
    let (head, sev) = match &u.link {
        Link::Up => u.status.headline(),
        Link::Connecting => ("CONNECTING", Severity::Info),
        Link::Stale(_) => ("NO DATA", Severity::Warn),
        Link::Down(_) => ("NO CONNECTION", Severity::Crit),
    };
    let mut lines = Vec::new();
    // Badge, then flags flowed after it.
    let mut row = vec![Span::styled(format!(" {head} "), t.badge(sev))];
    let mut used = head.len() + 2;
    if u.is_live() {
        for flag in u.status.extra_flags() {
            let (label, fsev, _) = flag_info(flag);
            let label = if label.is_empty() { flag.to_string() } else { label.to_string() };
            if used + label.len() + 2 > w {
                lines.push(Line::from(std::mem::take(&mut row)));
                used = 0;
            }
            row.push(Span::raw("  "));
            row.push(Span::styled(label.clone(), t.sev(fsev).add_modifier(ratatui::style::Modifier::BOLD)));
            used += label.len() + 2;
        }
    }
    lines.push(Line::from(row));

    match &u.link {
        Link::Down(e) | Link::Stale(e) => {
            lines.extend(wrap(e, w, t.sev(sev)));
            if let ServerLink::Down { retry_at, .. } = &app.servers[u.server].link {
                let left = retry_at.saturating_duration_since(std::time::Instant::now()).as_secs();
                lines.push(Line::from(dim(t, format!("retrying in {left}s"))));
            }
            if u.polls > 0 {
                let (last, _) = u.status.headline();
                lines.push(Line::from(dim(t, format!("last seen {}", last.to_lowercase()))));
            }
        }
        Link::Connecting => lines.push(Line::from(dim(t, format!("connecting to {}", u.server_label)))),
        Link::Up => {
            if let Some((age, seen)) = u.power_age() {
                let text = if seen {
                    format!("for {}, since {}", fmt::duration(age), clock_in(-age))
                } else {
                    format!("for at least {}", fmt::duration(age))
                };
                lines.push(Line::from(dim(t, text)));
            }
        }
    }
    if let Some(alarm) = u.text("ups.alarm") {
        lines.extend(wrap(&format!("Alarm: {alarm}"), w, t.sev(Severity::Crit)));
    }
    if let Some(r) = u.text("ups.test.result").filter(|r| *r != "No test initiated") {
        lines.push(kv(t, "Self-test", LW + 1, vec![val(t, r.to_string())]));
    }
    if u.outages > 0 || u.outage_start.is_some() {
        let what = match u.outages {
            0 => "in progress".to_string(),
            n => format!(
                "{n} in {}",
                fmt::duration(u.since.and_then(|s| s.elapsed().ok()).map_or(0.0, |d| d.as_secs_f64()))
            ),
        };
        let text = format!("{what}, {} on battery", fmt::duration(u.battery_total()));
        lines.push(kv(t, "Outages", LW + 1, vec![val(t, text)]));
    }
    lines
}

fn charge_severity(u: &Ups, charge: f64, low: Option<f64>, warn: Option<f64>) -> Severity {
    if low.is_some_and(|l| charge <= l) || u.status.has("LB") {
        Severity::Crit
    } else if warn.is_some_and(|w| charge <= w) || u.status.discharging() {
        Severity::Warn
    } else {
        Severity::Good
    }
}

fn battery(cx: &Ctx, u: &Ups, w: usize, extra: u16) -> Vec<Line<'static>> {
    let t = &cx.t;
    let mut lines = Vec::new();
    let charge = u.num("battery.charge");
    let low = u.num("battery.charge.low").filter(|v| *v > 0.0);
    let warn = u.num("battery.charge.warning").filter(|v| *v > 0.0);

    if let Some(c) = charge {
        let c = Charge { value: c, sev: charge_severity(u, c, low, warn), low, warn };
        lines.extend(charge::lines(t, cx.app.charge_style, &c, w, 1 + extra as usize, LW));
    }
    if let Some(r) = u.num("battery.runtime") {
        let mut v = vec![val(t, fmt::duration_long(r))];
        if let Some(l) = u.num("battery.runtime.low").filter(|l| *l > 0.0) {
            v.push(dim(t, format!("  low at {}", fmt::duration(l))));
        }
        lines.push(kv(t, "Runtime", LW, v));
    }

    let est = u.estimate(unix(SystemTime::now()));
    match &est {
        Some(e) => {
            let sev = if e.trend == Trend::Charging { Severity::Good } else { Severity::Warn };
            let rate = if e.per_hour.abs() >= 100.0 {
                format!("{:+.0}%/h", e.per_hour)
            } else {
                format!("{:+.1}%/h", e.per_hour)
            };
            let mut used = LW + rate.len();
            let mut v = vec![Span::styled(rate, t.sev(sev))];
            let target = match e.trend {
                Trend::Discharging => e.to_low.map(|s| ("low", s)).or(e.to_empty.map(|s| ("empty", s))),
                Trend::Charging => e.to_full.map(|s| ("full", s)),
            };
            if let Some((what, secs)) = target {
                let approx = if e.rough { "~" } else { "" };
                let when = format!(", {what} in {approx}{}", fmt::duration(secs));
                let at = format!(" at {}", clock_in(secs));
                used += when.len();
                v.push(val(t, when));
                if used + at.len() <= w {
                    v.push(dim(t, at));
                }
            }
            lines.push(kv(t, "Trend", LW, v));
        }
        None if u.is_live() && (u.status.discharging() || u.status.charging()) => {
            lines.push(kv(t, "Trend", LW, vec![dim(t, "measuring, waits for the charge to move")]));
        }
        None => {}
    }
    if let (Some(wh), Some(c)) = (u.capacity_wh, charge) {
        let mut v = vec![val(t, format!("~{} of {} Wh", fmt::num((wh * c / 100.0).round()), fmt::num(wh)))];
        if let Some(watts) = est.as_ref().and_then(|e| e.watts.map(|w| (w, e.trend))) {
            let dir = if watts.1 == Trend::Charging { "in" } else { "out" };
            v.push(dim(t, format!("  ~{} W {dir}", fmt::num(watts.0.round()))));
        }
        lines.push(kv(t, "Energy", LW, v));
    }
    if let Some(v) = u.text("battery.voltage") {
        let mut s = vec![val(t, fmt::raw_with_unit("battery.voltage", v))];
        if let Some(n) = u.text("battery.voltage.nominal") {
            s.push(dim(t, format!("  nominal {n} V")));
        }
        lines.push(kv(t, "Voltage", LW, s));
    }
    if let Some(v) = u.text("battery.current") {
        lines.push(kv(t, "Current", LW, vec![val(t, fmt::raw_with_unit("battery.current", v))]));
    }
    if let Some(v) = u.text("battery.temperature") {
        lines.push(kv(t, "Temp", LW, vec![val(t, fmt::raw_with_unit("battery.temperature", v))]));
    }
    let mut details = Vec::new();
    if let Some(ty) = u.text("battery.type") {
        details.push(ty.to_string());
    }
    if let Some(d) = u.text("battery.date").filter(|d| *d != "not set") {
        details.push(format!("installed {d}"));
    }
    if let Some(p) = u.text("battery.packs").filter(|p| *p != "1") {
        details.push(format!("{p} packs"));
    }
    if !details.is_empty() {
        lines.push(kv(t, "Type", LW, vec![dim(t, details.join(", "))]));
    }
    lines
}

fn power(t: &Theme, u: &Ups, w: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(load) = u.load() {
        let sev = if load >= 90.0 {
            Severity::Crit
        } else if load >= 70.0 {
            Severity::Warn
        } else {
            Severity::Good
        };
        let value = format!(" {:>4}", format!("{}%", fmt::num(load.round())));
        let bw = w.saturating_sub(LW + value.len());
        let mut spans = bar(t, bw, load / 100.0, t.sev(sev), &[]);
        spans.push(Span::styled(value, t.value().patch(t.sev(sev))));
        lines.push(kv(t, "Load", LW, spans));
    }
    let of = |var: &str, unit: &str| -> Option<Vec<Span<'static>>> {
        let v = u.text(var)?;
        let mut s = vec![val(t, format!("{v} {unit}"))];
        if let Some(n) = u.text(&format!("{var}.nominal")) {
            s.push(dim(t, format!(" of {n} {unit}")));
        }
        Some(s)
    };
    if let Some(s) = of("ups.realpower", "W") {
        lines.push(kv(t, "Real", LW, s));
    }
    if let Some(s) = of("ups.power", "VA") {
        lines.push(kv(t, "Apparent", LW, s));
    }
    if let Some(v) = u.text("output.current") {
        lines.push(kv(t, "Current", LW, vec![val(t, fmt::raw_with_unit("output.current", v))]));
    }
    if let Some(v) = u.text("ups.efficiency") {
        lines.push(kv(t, "Effic.", LW, vec![val(t, format!("{v}%"))]));
    }
    lines.extend(outlet_lines(t, u, w));
    lines
}

/// A numbered outlet or outlet group, as PDUs, rack UPSes and power
/// stations report them.
struct Outlet {
    name: String,
    /// What it draws, with the unit.
    draw: Option<(f64, &'static str)>,
    off: bool,
}

/// Outlets beyond these are left to the Variables view.
const MAX_OUTLETS: usize = 8;

fn outlets(u: &Ups) -> Vec<Outlet> {
    let mut ids: Vec<u32> =
        u.vars.keys().filter_map(|k| k.strip_prefix("outlet.")?.split('.').next()?.parse().ok()).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut list: Vec<Outlet> = ids
        .into_iter()
        .filter_map(|n| {
            let var = |what: &str| format!("outlet.{n}.{what}");
            let draw = [("realpower", "W"), ("power", "VA"), ("current", "A")]
                .into_iter()
                .find_map(|(what, unit)| Some((u.num(&var(what))?, unit)));
            let status = u.text(&var("status"));
            if draw.is_none() && status.is_none() {
                return None;
            }
            let name = u.text(&var("desc")).filter(|d| !d.trim().is_empty());
            Some(Outlet {
                name: name.map_or_else(|| format!("Outlet {n}"), |d| d.trim().to_string()),
                draw,
                off: status.is_some_and(|s| s.eq_ignore_ascii_case("off")),
            })
        })
        .collect();
    // "AC output", "DC output": a last word they all share says nothing.
    let last = |s: &str| s.rsplit_once(' ').map(|(_, l)| l.to_ascii_lowercase());
    if list.len() > 1
        && let Some(word) = last(&list[0].name)
        && list.iter().all(|o| last(&o.name).as_ref() == Some(&word))
    {
        for o in &mut list {
            if let Some((head, _)) = o.name.rsplit_once(' ') {
                o.name = head.trim_end().to_string();
            }
        }
    }
    list
}

/// A row per outlet: its name, its share of the total draw and the draw.
fn outlet_lines(t: &Theme, u: &Ups, w: usize) -> Vec<Line<'static>> {
    let list = outlets(u);
    let draws: Vec<String> = list
        .iter()
        .map(|o| match (o.off, o.draw) {
            (true, _) => "off".to_string(),
            (false, Some((v, unit))) => format!("{} {unit}", fmt::num(v)),
            (false, None) => "on".to_string(),
        })
        .collect();
    let vw = draws.iter().map(|d| d.chars().count()).max().unwrap_or(0);
    let nw = list.iter().map(|o| o.name.chars().count()).max().unwrap_or(0).min(14).min(w.saturating_sub(LW + 1 + vw));
    if nw == 0 {
        return Vec::new();
    }
    // Shares only add up when every outlet counts in the same unit.
    let unit = list.iter().find_map(|o| o.draw.map(|d| d.1));
    let same = list.iter().all(|o| o.draw.is_none_or(|d| Some(d.1) == unit));
    let total: f64 = list.iter().filter(|o| !o.off).filter_map(|o| o.draw).map(|d| d.0.max(0.0)).sum();
    let bw = w.saturating_sub(LW + nw + 2 + vw);
    let mut lines = Vec::new();
    for (i, (o, d)) in list.iter().zip(&draws).take(MAX_OUTLETS).enumerate() {
        let name: String = o.name.chars().take(nw).collect();
        let mut spans = vec![Span::raw(format!("{name:<nw$} "))];
        if bw >= 4 {
            let share = match o.draw {
                Some((v, _)) if same && total > 0.0 && !o.off => v.max(0.0) / total,
                _ => 0.0,
            };
            spans.extend(bar(t, bw, share, t.accent(), &[]));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(format!("{d:>vw$}"), if o.off { t.dim() } else { t.value() }));
        lines.push(kv(t, if i == 0 { "Outlets" } else { "" }, LW, spans));
    }
    if list.len() > MAX_OUTLETS {
        let more = format!("+{} more in Variables", list.len() - MAX_OUTLETS);
        lines.push(kv(t, "", LW, vec![dim(t, more)]));
    }
    lines
}

fn line(t: &Theme, u: &Ups, w: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let tl = u.num("input.transfer.low");
    let th = u.num("input.transfer.high");
    for (label, var) in [("Input", "input.voltage"), ("Output", "output.voltage")] {
        let Some(raw) = u.text(var) else { continue };
        let Some(v) = fmt::parse_num(raw) else { continue };
        let value = format!("{:<8}", fmt::raw_with_unit(var, raw));
        let mut spans = vec![val(t, value.clone())];
        let nominal = u.num(&format!("{var}.nominal")).or(u.num("input.voltage.nominal"));
        let zone = match (tl, th, nominal) {
            (Some(a), Some(b), _) if b > a && var == "input.voltage" => Some((a, b)),
            (_, _, Some(n)) if n > 0.0 => Some((n * 0.9, n * 1.1)),
            _ => None,
        };
        let mw = w.saturating_sub(LW + value.len() + 1);
        if let Some((a, b)) = zone
            && mw >= 8
        {
            let span = b - a;
            let inside = v >= a && v <= b;
            let sev = if inside {
                Severity::Good
            } else if v < 1.0 {
                Severity::Crit
            } else {
                Severity::Warn
            };
            spans.push(Span::raw(" "));
            spans.extend(meter(t, mw, v, a - span * 0.4, b + span * 0.4, (a, b), t.sev(sev)));
        }
        lines.push(kv(t, label, LW, spans));
    }
    let fin = u.text("input.frequency");
    let fout = u.text("output.frequency");
    if fin.is_some() || fout.is_some() {
        let mut parts = Vec::new();
        if let Some(f) = fin {
            parts.push(format!("{f} Hz in"));
        }
        if let Some(f) = fout {
            parts.push(format!("{f} Hz out"));
        }
        lines.push(kv(t, "Freq", LW, vec![val(t, parts.join(", "))]));
    }
    if let Some(v) = u.text("input.current") {
        lines.push(kv(t, "Current", LW, vec![val(t, format!("{v} A in"))]));
    }
    if let (Some(a), Some(b)) = (u.text("input.transfer.low"), u.text("input.transfer.high")) {
        lines.push(kv(t, "Transfer", LW, vec![dim(t, format!("{a}..{b} V"))]));
    }
    if let Some(s) = u.text("input.sensitivity") {
        lines.push(kv(t, "Sensitiv.", LW, vec![dim(t, s.to_string())]));
    }
    if let Some(r) = u.text("input.transfer.reason") {
        for (i, l) in wrap(r, w.saturating_sub(LW), t.dim()).into_iter().enumerate() {
            lines.push(kv(t, if i == 0 { "Last xfer" } else { "" }, LW, l.spans));
        }
    }
    lines
}

fn environment(t: &Theme, u: &Ups, w: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(v) = u.text("ambient.temperature") {
        let mut s = vec![val(t, fmt::raw_with_unit("ambient.temperature", v))];
        if let (Some(lo), Some(hi)) = (u.text("ambient.temperature.low"), u.text("ambient.temperature.high")) {
            s.push(dim(t, format!("  limits {lo}..{hi} C")));
        }
        lines.push(kv(t, "Ambient", LW, s));
    }
    if let Some(h) = u.num("ambient.humidity") {
        let value = format!(" {:>4}", format!("{}%", fmt::num(h.round())));
        let bw = w.saturating_sub(LW + value.len());
        let mut s = bar(t, bw, h / 100.0, t.accent(), &[]);
        s.push(val(t, value));
        lines.push(kv(t, "Humidity", LW, s));
    }
    if let Some(v) = u.text("ups.temperature") {
        lines.push(kv(t, "UPS temp", LW, vec![val(t, fmt::raw_with_unit("ups.temperature", v))]));
    }
    lines
}

const TREND_VARS: [(&str, &str); 8] = [
    ("battery.charge", "Charge"),
    ("ups.load", "Load"),
    ("ups.realpower", "Power"),
    ("battery.runtime", "Runtime"),
    ("input.voltage", "Input"),
    ("battery.voltage", "Batt V"),
    ("output.voltage", "Output"),
    ("battery.temperature", "Temp"),
];

fn trend_metrics(u: &Ups) -> Vec<(&'static str, &'static str)> {
    TREND_VARS.iter().copied().filter(|(v, _)| u.history.get(v).is_some_and(|s| s.len() > 0)).collect()
}

fn trends(cx: &Ctx, u: &Ups, w: usize, extra: u16) -> Vec<Line<'static>> {
    let t = &cx.t;
    let metrics = trend_metrics(u);
    if metrics.is_empty() {
        return Vec::new();
    }
    let budget = metrics.len().min(3) + extra as usize;
    let shown = metrics.len().min(budget);
    let per = (budget / shown).clamp(1, 3);
    let now = unix(SystemTime::now());
    let window = WINDOWS[cx.app.hist.window];
    let vw = 9;
    let sw = w.saturating_sub(LW + vw);
    let mut lines = Vec::new();
    for (var, label) in &metrics[..shown] {
        let series = u.history.get(var).unwrap();
        let cols = series.columns(now - window, now, sw);
        let percent = fmt::unit(var) == "%";
        let (lo, hi) =
            if percent { (0.0, 100.0_f64.max(spark_range(&cols, true).1)) } else { spark_range(&cols, false) };
        let style = if *var == "battery.charge" {
            let c = u.num(var).unwrap_or(100.0);
            t.sev(charge_severity(u, c, u.num("battery.charge.low").filter(|v| *v > 0.0), None))
        } else {
            t.accent()
        };
        let rows = sparkline(t, &cols, lo, hi, per, style);
        let value = u.get(var).map(|v| fmt::raw_short(var, v)).unwrap_or_default();
        let n = rows.len();
        for (i, row) in rows.into_iter().enumerate() {
            let last = i == n - 1;
            let mut spans =
                vec![if last { Span::styled(format!("{label:<LW$}"), t.label()) } else { Span::raw(" ".repeat(LW)) }];
            spans.extend(row.spans);
            if last {
                spans.push(val(t, format!(" {value:>width$}", width = vw - 1)));
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn device(cx: &Ctx, u: &Ups, w: usize) -> Vec<Line<'static>> {
    let t = &cx.t;
    let mut lines = Vec::new();
    let either = |a: &str, b: &str| u.text(a).or(u.text(b)).map(str::to_string);
    let mut row = |label: &str, v: Option<String>| {
        if let Some(v) = v {
            let room = w.saturating_sub(LW);
            let v = if v.chars().count() > room && room > 3 {
                format!("{}...", v.chars().take(room - 3).collect::<String>())
            } else {
                v
            };
            lines.push(kv(t, label, LW, vec![val(t, v)]));
        }
    };
    row("Model", either("device.model", "ups.model"));
    row("Maker", either("device.mfr", "ups.mfr"));
    row("Serial", either("device.serial", "ups.serial"));
    row(
        "Firmware",
        u.text("ups.firmware").map(|f| match u.text("ups.firmware.aux") {
            Some(aux) => format!("{f} / {aux}"),
            None => f.to_string(),
        }),
    );
    let driver = u.text("driver.name").map(|n| {
        let mut s = n.to_string();
        if let Some(v) = u.text("driver.version") {
            s += &format!(" {v}");
        }
        if let Some(d) = u.text("driver.version.data") {
            s += &format!(" ({d})");
        }
        if let Some(st) = u.text("driver.state").filter(|s| *s != "quiet") {
            s += &format!(", {st}");
        }
        s
    });
    row("Driver", driver);
    row("Beeper", u.text("ups.beeper.status").map(str::to_string));
    let watts = u.text("ups.realpower.nominal").map(|v| format!("{v} W"));
    let size = if u.capacity_guessed {
        u.capacity_wh.map(|wh| format!("{} Wh pack", fmt::num(wh)))
    } else {
        u.text("ups.power.nominal").map(|v| format!("{v} VA"))
    };
    let rated = match (size, watts) {
        (Some(a), Some(b)) => Some(format!("{a}, {b}")),
        (a, b) => a.or(b),
    };
    row("Rated", rated);
    let srv = &cx.app.servers[u.server];
    let server = match &srv.version {
        Some(v) => format!("{}, {v}", u.server_label),
        None => u.server_label.clone(),
    };
    row("Server", Some(server));
    row("Recorded", u.recorded.as_ref().map(|(via, since)| format!("since {}, {via}", fmt::when(*since))));
    let model = either("device.model", "ups.model").unwrap_or_default();
    row("Desc", Some(u.desc.clone()).filter(|d| !d.is_empty() && *d != model));
    if !u.cmds.is_empty() || !u.rw.is_empty() {
        let mut parts = Vec::new();
        if !u.cmds.is_empty() {
            let n = u.cmds.len();
            parts.push(format!("{n} command{} (c)", if n == 1 { "" } else { "s" }));
        }
        if !u.rw.is_empty() {
            parts.push(format!("{} writable", u.rw.len()));
        }
        row("Control", Some(parts.join(", ")));
    }
    lines
}

fn events(cx: &Ctx, u: &Ups, w: usize, extra: u16) -> Vec<Line<'static>> {
    let t = &cx.t;
    let rows = 4 + extra as usize;
    let mine: Vec<_> =
        cx.app.log.iter().rev().filter(|e| e.ups == u.label || e.ups == u.server_label).take(rows).collect();
    if mine.is_empty() {
        return vec![Line::from(dim(t, "nothing yet"))];
    }
    mine.into_iter()
        .map(|e| {
            let time = e.at.format("%H:%M:%S").to_string();
            let room = w.saturating_sub(time.len() + 3);
            let text: String = if e.text.chars().count() > room && room > 3 {
                format!("{}...", e.text.chars().take(room - 3).collect::<String>())
            } else {
                e.text.clone()
            };
            Line::from(vec![
                Span::styled(t.stripe(), t.sev(e.severity)),
                Span::styled(format!("{time} "), t.dim()),
                Span::styled(text, if e.severity >= Severity::Warn { t.sev(e.severity) } else { Style::new() }),
            ])
        })
        .collect()
}

/// Borderless summary for panes too small for cards, e.g. a few rows
/// of a tmux split kept as a status monitor.
fn compact(f: &mut Frame, area: Rect, cx: &Ctx, u: &Ups) {
    let t = &cx.t;
    let w = area.width as usize;
    let lw = 5;
    let (head, sev) = match &u.link {
        Link::Up => u.status.headline(),
        Link::Connecting => ("CONNECTING", Severity::Info),
        Link::Stale(_) => ("NO DATA", Severity::Warn),
        Link::Down(_) => ("NO CONNECTION", Severity::Crit),
    };
    let mut first = vec![Span::styled(format!(" {head} "), t.badge(sev))];
    let mut used = head.len() + 2;
    for flag in u.status.extra_flags().filter(|_| u.is_live()) {
        let (label, fsev, _) = flag_info(flag);
        let label = if label.is_empty() { flag } else { label };
        if used + label.len() + 1 > w {
            break;
        }
        first.push(Span::styled(format!(" {label}"), t.sev(fsev)));
        used += label.len() + 1;
    }
    let mut lines = vec![Line::from(first)];
    if let Some(c) = u.num("battery.charge") {
        let low = u.num("battery.charge.low").filter(|v| *v > 0.0);
        let s = charge_severity(u, c, low, None);
        let value = format!(" {:>4}", format!("{}%", fmt::num(c.round())));
        let mut spans = bar(t, w.saturating_sub(lw + value.len()), c / 100.0, t.sev(s), &[]);
        spans.push(val(t, value));
        lines.push(kv(t, "Batt", lw, spans));
    }
    let est = u.estimate(unix(SystemTime::now()));
    let mut run = Vec::new();
    if let Some(r) = u.num("battery.runtime") {
        run.push(val(t, fmt::duration(r)));
    }
    if let Some(e) = &est {
        let target = match e.trend {
            Trend::Discharging => e.to_low.map(|s| ("low", s)).or(e.to_empty.map(|s| ("empty", s))),
            Trend::Charging => e.to_full.map(|s| ("full", s)),
        };
        if let Some((what, secs)) = target {
            run.push(dim(t, format!("  {what} in ~{}", fmt::duration(secs))));
        }
    }
    if !run.is_empty() {
        lines.push(kv(t, "Run", lw, run));
    }
    if let Some(l) = u.load() {
        let mut s = vec![val(t, format!("{}%", fmt::num(l.round())))];
        if let Some(p) = u.text("ups.realpower") {
            s.push(dim(t, format!("  {p} W")));
        }
        lines.push(kv(t, "Load", lw, s));
    }
    let mut line = Vec::new();
    if let Some(v) = u.text("input.voltage") {
        line.push(val(t, format!("{v} V in")));
    }
    if let Some(v) = u.text("output.voltage") {
        line.push(dim(t, format!("  {v} V out")));
    }
    if !line.is_empty() {
        lines.push(kv(t, "Line", lw, line));
    }
    if let Some(e) = cx.app.log.iter().rev().find(|e| e.ups == u.label || e.ups == u.server_label) {
        let text = format!("{} {}", e.at.format("%H:%M"), e.text);
        lines.push(Line::from(vec![Span::styled(t.stripe(), t.sev(e.severity)), dim(t, text)]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_in_order() {
        let cols = pack(&[8, 9, 6, 6, 6, 9, 6], 2, 40).unwrap();
        assert_eq!(cols, vec![vec![0, 1, 2], vec![3, 4, 5, 6]]);
        assert!(pack(&[30, 30, 30], 2, 40).is_none());
        // Balanced: two short cards go side by side rather than stacked.
        assert_eq!(pack(&[5, 5], 3, 10).unwrap(), vec![vec![0], vec![1]]);
    }

    fn ups(vars: &[(&str, &str)]) -> Ups {
        let mut u = Ups::new(0, "localhost", "ups", "", crate::history::History::new(3600.0, 2.0), None);
        u.apply(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), SystemTime::now());
        u
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn lists_outlets() {
        let t = Theme { color: false, ascii: false };
        // A power station: the shared "output" goes, every outlet is listed.
        let u = ups(&[
            ("ups.status", "OL"),
            ("outlet.count", "3"),
            ("outlet.1.desc", "AC output"),
            ("outlet.1.realpower", "113.0"),
            ("outlet.2.desc", "DC output"),
            ("outlet.2.realpower", "0.0"),
            ("outlet.10.desc", "USB-C output"),
            ("outlet.10.realpower", "2.5"),
        ]);
        assert!(cards(&u).contains(&Card::Power));
        let rows = text(&outlet_lines(&t, &u, 40));
        assert_eq!(rows.len(), 3);
        assert!(rows[0].starts_with("Outlets   AC  ") && rows[0].ends_with(" 113 W"), "{rows:?}");
        assert!(rows[1].starts_with("          DC  ") && rows[1].ends_with("   0 W"), "{rows:?}");
        assert!(rows[2].starts_with("          USB-C ") && rows[2].ends_with("2.50 W"), "{rows:?}");
        assert!(rows.iter().all(|r| r.chars().count() <= 40));
        // Too narrow for a bar, still a name and a value; too narrow for that, nothing.
        assert_eq!(text(&outlet_lines(&t, &u, 24))[0], "Outlets   AC     113 W");
        assert!(outlet_lines(&t, &u, 12).is_empty());

        // A switched outlet with no reading, and one without a name.
        let u = ups(&[("ups.status", "OL"), ("outlet.1.status", "off"), ("outlet.2.current", "1.20")]);
        let rows = text(&outlet_lines(&t, &u, 40));
        assert!(rows[0].starts_with("Outlets   Outlet 1") && rows[0].ends_with("off"), "{rows:?}");
        assert!(rows[1].ends_with("1.20 A"), "{rows:?}");
        // Only a description is not an outlet worth a row.
        assert!(outlets(&ups(&[("outlet.1.desc", "Main")])).is_empty());
    }

    #[test]
    fn shares_spare_rows() {
        assert_eq!(share(&[0, 2, 0, 3], 10), vec![0, 4, 0, 6]);
        assert_eq!(share(&[0, 0], 5), vec![0, 5]);
        assert_eq!(share(&[], 5), Vec::<u16>::new());
    }
}
