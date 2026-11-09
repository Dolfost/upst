//! Every variable the UPS reports, filterable, with a detail pane that
//! shows upsd's description, type, change history and a chart.

use std::time::{Duration, SystemTime};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Cell, Chart, Dataset, GraphType, Paragraph, Row, ScrollbarState, Table, TableState};

use super::Ctx;
use super::overview::wrap;
use super::widgets::kv;
use crate::app::{Hit, WINDOWS};
use crate::fmt;
use crate::model::{Ups, unix};

/// How long a changed value stays highlighted.
const FRESH: Duration = Duration::from_secs(6);

pub fn draw(f: &mut Frame, area: Rect, cx: &mut Ctx, u: &Ups, state: &mut TableState) {
    let t = cx.t;
    let names = cx.app.visible_vars();
    let (list_area, detail_area) = if area.width >= 110 {
        let [a, b] = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(area);
        (a, Some(b))
    } else if area.height >= 26 {
        let [a, b] = Layout::vertical([Constraint::Fill(1), Constraint::Length(12)]).areas(area);
        (a, Some(b))
    } else {
        (area, None)
    };

    let filter = &cx.app.vars.filter.text;
    let title = if filter.is_empty() {
        format!(" Variables {} ", names.len())
    } else {
        format!(" Variables {}/{} matching \"{filter}\" ", names.len(), u.vars.len())
    };
    let block = t.block(true).title(title);
    let inner = block.inner(list_area);

    let name_w = names.iter().map(|n| n.len()).max().unwrap_or(10).clamp(10, (inner.width as usize * 6 / 10).max(10));
    let rows: Vec<Row> = names
        .iter()
        .map(|name| {
            let var = &u.vars[name];
            let (group, rest) = name.split_once('.').unwrap_or((name, ""));
            let mut spans = vec![Span::styled(group.to_string(), t.group(group))];
            if !rest.is_empty() {
                spans.push(Span::raw(format!(".{rest}")));
            }
            let fresh = var.changed.is_some_and(|c| c.elapsed() < FRESH);
            let value_style = if fresh { t.fresh() } else { Style::new() };
            let rw = if u.rw.contains(name) { Span::styled("rw", t.rw()) } else { Span::raw("") };
            Row::new(vec![
                Cell::from(Line::from(spans)),
                Cell::from(rw),
                Cell::from(Span::styled(var.value.clone(), value_style)),
            ])
        })
        .collect();
    let n = rows.len();
    if let Some(sel) = state.selected()
        && sel >= n
    {
        state.select(n.checked_sub(1));
    }
    if state.selected().is_none() && n > 0 {
        state.select(Some(0));
    }
    let table = Table::new(rows, [Constraint::Length(name_w as u16), Constraint::Length(2), Constraint::Fill(1)])
        .block(block)
        .column_spacing(1)
        .row_highlight_style(t.selected());
    f.render_stateful_widget(table, list_area, state);

    let offset = state.offset();
    for i in 0..(inner.height as usize).min(n.saturating_sub(offset)) {
        cx.hit(Rect::new(inner.x, inner.y + i as u16, inner.width, 1), Hit::VarRow(offset + i));
    }
    if n > inner.height as usize {
        let mut sb = ScrollbarState::new(n.saturating_sub(inner.height as usize)).position(offset);
        f.render_stateful_widget(t.scrollbar(), list_area.inner(ratatui::layout::Margin::new(0, 1)), &mut sb);
    }
    if n == 0 {
        let msg = if u.vars.is_empty() { "No variables yet" } else { "Nothing matches the filter" };
        f.render_widget(Paragraph::new(Span::styled(msg, t.dim())).centered(), inner);
    }

    if let (Some(area), Some(sel)) = (detail_area, state.selected())
        && let Some(name) = names.get(sel)
    {
        detail(f, area, cx, u, name);
    }
}

fn detail(f: &mut Frame, area: Rect, cx: &Ctx, u: &Ups, name: &str) {
    let t = cx.t;
    let block = t.block(false).title(format!(" {name} "));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let lw = 9;
    let var = &u.vars[name];
    let info = u.info.get(name);
    let mut lines = Vec::new();

    let mut value = vec![Span::styled(var.value.clone(), t.value())];
    match (fmt::unit(name), fmt::parse_num(&var.value)) {
        ("", _) | (_, None) => {}
        ("s", Some(_)) => value.push(Span::styled(format!("  ({})", fmt::raw_with_unit(name, &var.value)), t.dim())),
        (u, Some(_)) => value.push(Span::styled(format!(" {u}"), t.dim())),
    }
    lines.push(kv(&t, "Value", lw, value));
    match info {
        Some(i) => {
            if let Some(d) = &i.desc {
                for (k, l) in wrap(d, w.saturating_sub(lw), Style::new()).into_iter().enumerate() {
                    let label = if k == 0 { "Meaning" } else { "" };
                    lines.push(kv(&t, label, lw, l.spans));
                }
            }
            if !i.kind.is_empty() {
                lines.push(kv(&t, "Type", lw, vec![Span::styled(i.kind.join(" "), t.dim())]));
            }
            if !i.enum_values.is_empty() {
                let text = i.enum_values.join(", ");
                for (k, l) in wrap(&text, w.saturating_sub(lw), t.dim()).into_iter().enumerate() {
                    lines.push(kv(&t, if k == 0 { "Choices" } else { "" }, lw, l.spans));
                }
            }
            if !i.ranges.is_empty() {
                let r: Vec<String> = i.ranges.iter().map(|(a, b)| format!("{a}..{b}")).collect();
                lines.push(kv(&t, "Range", lw, vec![Span::styled(r.join(", "), t.dim())]));
            }
        }
        None if u.info_pending.contains(name) => {
            lines.push(kv(&t, "Meaning", lw, vec![Span::styled("asking upsd...", t.dim())]))
        }
        None => {}
    }
    let changed = match (&var.changed, &var.prev) {
        (Some(at), Some(prev)) => format!("{}, was {prev}", fmt::ago(at.elapsed())),
        (Some(at), None) => format!("appeared {}", fmt::ago(at.elapsed())),
        _ => "not since upst started".to_string(),
    };
    lines.push(kv(&t, "Changed", lw, vec![Span::styled(changed, t.dim())]));
    let series = u.history.get(name);
    if let Some(s) = series
        && let (Some(first), Some((lo, hi))) = (s.first(), s.min_max(f64::MIN, f64::MAX))
        && s.varied()
    {
        let since = unix(SystemTime::now()) - first.0;
        let text =
            format!("{} .. {} over {}", fmt::with_unit(name, lo), fmt::with_unit(name, hi), fmt::duration(since));
        lines.push(kv(&t, "Seen", lw, vec![Span::styled(text, t.dim())]));
    }
    if u.rw.contains(name) {
        lines.push(kv(&t, "", lw, vec![Span::styled("enter or e to change", t.rw())]));
    }

    let text_h = (lines.len() as u16).min(inner.height);
    let [text_area, chart_area] = Layout::vertical([Constraint::Length(text_h), Constraint::Fill(1)]).areas(inner);
    f.render_widget(Paragraph::new(lines), text_area);

    // A chart in whatever room is left, for numbers that moved.
    if let Some(s) = series
        && s.varied()
        && chart_area.height >= 5
    {
        let now = unix(SystemTime::now());
        let window = WINDOWS[cx.app.hist.window];
        let pts: Vec<(f64, f64)> = s
            .chart_points(now - window, now, chart_area.width as usize * 2)
            .into_iter()
            .map(|(x, y)| (x - now, y))
            .collect();
        if let Some((lo, hi)) = s.min_max(now - window, now) {
            let pad = ((hi - lo) * 0.1).max(hi.abs() * 0.01).max(0.5);
            let label = |v: f64| Span::styled(fmt::with_unit(name, v), t.dim());
            let chart = Chart::new(vec![
                Dataset::default().data(&pts).graph_type(GraphType::Line).marker(t.marker()).style(t.accent()),
            ])
            .x_axis(
                Axis::default()
                    .bounds([-window, 0.0])
                    .labels(vec![
                        Span::styled(format!("-{}", fmt::span(window)), t.dim()),
                        Span::styled("now", t.dim()),
                    ])
                    .style(t.dim()),
            )
            .y_axis(Axis::default().bounds([lo - pad, hi + pad]).labels(vec![label(lo), label(hi)]).style(t.dim()))
            .legend_position(None);
            f.render_widget(chart, chart_area);
        }
    }
}
