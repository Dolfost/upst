//! Charts of every interesting series over a selectable window, plus a
//! ribbon showing where the power came from.

use std::time::SystemTime;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, Paragraph};

use super::{Ctx, Theme};
use crate::app::{Hit, WINDOWS};
use crate::fmt;
use crate::history::{Band, Series};
use crate::model::{Ups, unix};
use crate::status::Severity;

/// Preferred order, with friendly titles.
const KNOWN: [(&str, &str); 18] = [
    ("battery.charge", "Battery charge"),
    ("battery.runtime", "Runtime"),
    ("ups.load", "Load"),
    ("ups.realpower", "Real power"),
    ("ups.power", "Apparent power"),
    ("battery.voltage", "Battery voltage"),
    ("input.voltage", "Input voltage"),
    ("output.voltage", "Output voltage"),
    ("input.frequency", "Input frequency"),
    ("output.frequency", "Output frequency"),
    ("input.current", "Input current"),
    ("output.current", "Output current"),
    ("battery.current", "Battery current"),
    ("battery.temperature", "Battery temperature"),
    ("ups.temperature", "UPS temperature"),
    ("ambient.temperature", "Ambient temperature"),
    ("ambient.humidity", "Ambient humidity"),
    ("ups.efficiency", "Efficiency"),
];

pub fn title(var: &str) -> &str {
    KNOWN.iter().find(|(v, _)| *v == var).map_or(var, |(_, t)| t)
}

/// Known series first, then anything else that moved.
pub fn metrics(u: &Ups) -> Vec<String> {
    let mut out: Vec<String> = KNOWN
        .iter()
        .filter(|(v, _)| u.history.get(v).is_some_and(|s| s.len() > 0))
        .map(|(v, _)| v.to_string())
        .collect();
    let mut others: Vec<String> = u
        .history
        .names()
        .filter(|n| !KNOWN.iter().any(|(k, _)| k == n))
        .filter(|n| u.history.get(n).is_some_and(Series::varied))
        .map(str::to_string)
        .collect();
    others.sort();
    out.extend(others);
    out
}

/// Returns how many charts exist, for selection wrap-around.
pub fn draw(f: &mut Frame, area: Rect, cx: &mut Ctx, u: &Ups) -> usize {
    let t = cx.t;
    let list = metrics(u);
    let [charts, ribbon] = Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(area);
    let window = WINDOWS[cx.app.hist.window];
    let now = unix(SystemTime::now());
    draw_ribbon(f, ribbon, &t, u, now, window);
    if list.is_empty() {
        let msg = Paragraph::new(Span::styled("No numeric data yet", t.dim())).centered();
        f.render_widget(msg, Rect { y: charts.y + charts.height / 2, height: 1, ..charts });
        return 0;
    }
    let sel = cx.app.hist.selected.min(list.len() - 1);

    if cx.app.hist.zoomed {
        chart(f, charts, &t, u, &list[sel], now, window, true);
        cx.hit(charts, Hit::Chart(sel));
        return list.len();
    }

    let cols = if charts.width >= 150 {
        3
    } else if charts.width >= 90 {
        2
    } else {
        1
    };
    let cols = cols.min(list.len());
    let min_h = 9;
    let fit_rows = (charts.height / min_h).max(1) as usize;
    let per_page = fit_rows * cols;
    let page = sel / per_page;
    let shown = &list[page * per_page..list.len().min((page + 1) * per_page)];
    let rows = shown.len().div_ceil(cols);
    let row_areas = Layout::vertical(vec![Constraint::Fill(1); rows]).split(charts);
    for (r, row) in row_areas.iter().enumerate() {
        // A short last row spreads its charts over the full width.
        let in_row = (shown.len() - r * cols).min(cols);
        let col_areas = Layout::horizontal(vec![Constraint::Fill(1); in_row]).split(*row);
        for (c, rect) in col_areas.iter().enumerate() {
            let k = r * cols + c;
            let Some(var) = shown.get(k) else { break };
            let idx = page * per_page + k;
            chart(f, *rect, &t, u, var, now, window, idx == sel);
            cx.hit(*rect, Hit::Chart(idx));
        }
    }
    let pages = list.len().div_ceil(per_page);
    if pages > 1 && charts.height > 0 {
        let note = format!(" page {}/{} ", page + 1, pages);
        let r = Rect::new(charts.x + 2, charts.y + charts.height - 1, note.len() as u16, 1);
        f.render_widget(Paragraph::new(Span::styled(note, t.dim())), r);
    }
    list.len()
}

/// Tick label: durations as `45m` or `1h 15m` rather than `45m 00s`.
fn axis_label(var: &str, v: f64) -> String {
    if fmt::unit(var) == "s" { fmt::span(v) } else { fmt::with_unit(var, v) }
}

/// A round step that cuts `span` into at most four parts; durations step
/// in clock units.
fn nice_step(span: f64, unit: &str) -> f64 {
    if unit == "s" {
        const STEPS: [f64; 16] = [
            1.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0, 21600.0, 43200.0,
            86400.0,
        ];
        return STEPS
            .iter()
            .copied()
            .find(|s| span / s <= 4.0)
            .unwrap_or_else(|| (span / 4.0 / 86400.0).ceil() * 86400.0);
    }
    let raw = span / 4.0;
    let mag = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(10.0 * mag)
}

/// Y bounds on round numbers, never narrower than a sensible span, and
/// the tick values to label.
fn bounds(var: &str, lo: f64, hi: f64) -> (f64, f64, Vec<f64>) {
    let unit = fmt::unit(var);
    if unit == "%" && hi <= 100.0 && lo >= 0.0 {
        return (0.0, 100.0, vec![0.0, 50.0, 100.0]);
    }
    let mid = (lo + hi) / 2.0;
    let min_span = match unit {
        "V" => 4.0,
        "Hz" => 1.0,
        "C" => 2.0,
        "s" => 120.0,
        "W" | "VA" => 20.0,
        "A" => 0.5,
        _ => (mid.abs() * 0.05).max(1.0),
    };
    let span = (hi - lo).max(min_span);
    let (lo, hi) = (lo.min(mid - span / 2.0), hi.max(mid + span / 2.0));
    let step = nice_step(span * 1.1, unit);
    let mut a = (lo / step).floor() * step;
    let mut b = (hi / step).ceil() * step;
    if b - a < step {
        b = a + step;
    }
    if lo >= 0.0 && a < 0.0 {
        a = 0.0;
    }
    let n = ((b - a) / step).round() as usize;
    let ticks = if n <= 4 { (0..=n).map(|i| a + step * i as f64).collect() } else { vec![a, (a + b) / 2.0, b] };
    if b <= a {
        b = a + 1.0;
    }
    (a, b, ticks)
}

fn thresholds(u: &Ups, var: &str) -> Vec<(f64, Severity, &'static str)> {
    let pick = |k: &str, s, label| u.num(k).filter(|v| *v > 0.0).map(|v| (v, s, label));
    match var {
        "battery.charge" => [
            pick("battery.charge.low", Severity::Crit, "low"),
            pick("battery.charge.warning", Severity::Warn, "warning"),
        ]
        .into_iter()
        .flatten()
        .collect(),
        "battery.runtime" => pick("battery.runtime.low", Severity::Crit, "low").into_iter().collect(),
        "input.voltage" => [
            pick("input.transfer.low", Severity::Warn, "transfer"),
            pick("input.transfer.high", Severity::Warn, "transfer"),
        ]
        .into_iter()
        .flatten()
        .collect(),
        _ => Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn chart(f: &mut Frame, area: Rect, t: &Theme, u: &Ups, var: &str, now: f64, window: f64, selected: bool) {
    let Some(series) = u.history.get(var) else { return };
    let t0 = now - window;
    let current = u.get(var).map(|v| fmt::raw_with_unit(var, v)).unwrap_or_default();
    let mut block = t.block(selected).title(Line::from(vec![
        Span::styled(format!(" {} ", title(var)), t.title()),
        Span::styled(format!("{current} "), t.accent()),
    ]));
    let points: Vec<(f64, f64)> =
        series.chart_points(t0, now, area.width as usize * 2).into_iter().map(|(x, y)| (x - now, y)).collect();
    let Some((lo, hi)) = series.min_max(t0, now) else {
        f.render_widget(block, area);
        return;
    };
    block = block.title_bottom(
        Line::from(Span::styled(
            format!(" min {}  max {} ", fmt::with_unit(var, lo), fmt::with_unit(var, hi)),
            t.dim(),
        ))
        .right_aligned(),
    );
    let (ylo, yhi, mut ticks) = bounds(var, lo, hi);
    // Labels need a row each and some air between them.
    if ticks.len() > 3 && (ticks.len() as u16) * 2 > area.height.saturating_sub(4) {
        ticks = vec![ylo, (ylo + yhi) / 2.0, yhi];
    }

    let lines: Vec<(Vec<(f64, f64)>, Style)> = thresholds(u, var)
        .into_iter()
        .filter(|(v, _, _)| *v >= ylo && *v <= yhi)
        .map(|(v, s, _)| (vec![(-window, v), (0.0, v)], t.sev(s)))
        .collect();

    let style = match var {
        "battery.charge" => t.sev(Severity::Good),
        _ => t.accent(),
    };
    let mut datasets: Vec<Dataset> = lines
        .iter()
        .map(|(pts, s)| Dataset::default().data(pts).graph_type(GraphType::Line).marker(t.dashed()).style(*s))
        .collect();
    datasets.push(Dataset::default().data(&points).graph_type(GraphType::Line).marker(t.marker()).style(style));

    let ylabel = |v: f64| Span::styled(axis_label(var, v), t.dim());
    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(
            Axis::default()
                .bounds([-window, 0.0])
                .labels(vec![
                    Span::styled(format!("-{}", fmt::span(window)), t.dim()),
                    Span::styled(format!("-{}", fmt::span(window / 2.0)), t.dim()),
                    Span::styled("now", t.dim()),
                ])
                .style(t.dim()),
        )
        .y_axis(
            Axis::default().bounds([ylo, yhi]).labels(ticks.into_iter().map(ylabel).collect::<Vec<_>>()).style(t.dim()),
        )
        .legend_position(None);
    f.render_widget(chart, area);
}

fn draw_ribbon(f: &mut Frame, area: Rect, t: &Theme, u: &Ups, now: f64, window: f64) {
    let block = t.block(false).title(" Power source ").title(
        Line::from(vec![
            Span::styled(t.band(Band::Online).0, t.band(Band::Online).1),
            Span::styled(" grid  ", t.dim()),
            Span::styled(t.band(Band::Battery).0, t.band(Band::Battery).1),
            Span::styled(" battery  ", t.dim()),
            Span::styled(t.band(Band::Low).0, t.band(Band::Low).1),
            Span::styled(" low  ", t.dim()),
            Span::styled(format!("window {} (+/-) ", fmt::span(window)), t.dim()),
        ])
        .right_aligned(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let cols = u.history.band_columns(now - window, now, inner.width as usize, now);
    let spans: Vec<Span> = cols
        .into_iter()
        .map(|b| {
            let (g, s) = t.band(b);
            Span::styled(g, s)
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_bounds() {
        let (a, b, ticks) = bounds("ups.realpower", 357.3, 485.7);
        assert_eq!((a, b), (350.0, 500.0));
        assert_eq!(ticks, [350.0, 400.0, 450.0, 500.0]);
        let (a, b, _) = bounds("input.voltage", 229.6, 230.9);
        assert!(a <= 228.0 && b >= 232.0 && a % 1.0 == 0.0);
        let (a, b, _) = bounds("battery.runtime", 1050.0, 1758.0);
        assert_eq!((a % 60.0, b % 60.0), (0.0, 0.0));
        assert_eq!(bounds("battery.charge", 47.0, 92.0).0, 0.0);
    }
}
