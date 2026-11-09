//! The event log, newest first.

use chrono::Local;
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, ScrollbarState};

use super::Ctx;
use crate::status::Severity;

pub fn draw(f: &mut Frame, area: Rect, cx: &mut Ctx) {
    let t = cx.t;
    let app = cx.app;
    let current = app.current();
    let entries: Vec<_> = app
        .log
        .iter()
        .rev()
        .filter(|e| app.events.all || current.is_some_and(|u| e.ups == u.label || e.ups == u.server_label))
        .collect();
    let scope = if app.events.all { "all" } else { "this UPS" };
    let block = t.block(true).title(format!(" Events {} ({scope}) ", entries.len()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if entries.is_empty() {
        f.render_widget(Paragraph::new(Span::styled("Nothing has happened yet", t.dim())).centered(), inner);
        return;
    }
    let show_ups = app.ups.len() > 1 || app.servers.len() > 1;
    let ups_w = entries.iter().map(|e| short_label(app, &e.ups).chars().count()).max().unwrap_or(0).min(28);
    let today = Local::now().date_naive();
    let old = entries.iter().any(|e| e.at.date_naive() != today);
    let tw = if old { 11 } else { 8 };
    let offset = app.events.offset.min(entries.len().saturating_sub(1));
    let lines: Vec<Line> = entries
        .iter()
        .skip(offset)
        .take(inner.height as usize)
        .map(|e| {
            let time = if e.at.date_naive() == today {
                e.at.format("%H:%M:%S").to_string()
            } else {
                e.at.format("%m-%d %H:%M").to_string()
            };
            let mut spans =
                vec![Span::styled(t.stripe(), t.sev(e.severity)), Span::styled(format!("{time:>tw$} "), t.dim())];
            if show_ups {
                let name = short_label(app, &e.ups);
                let name: String = name.chars().take(ups_w).collect();
                spans.push(Span::styled(format!("{name:<ups_w$}  "), t.accent()));
            }
            let style = if e.severity >= Severity::Warn { t.sev(e.severity) } else { Style::new() };
            spans.push(Span::styled(e.text.clone(), style));
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
    if entries.len() > inner.height as usize {
        let mut sb = ScrollbarState::new(entries.len().saturating_sub(inner.height as usize)).position(offset);
        f.render_stateful_widget(t.scrollbar(), area.inner(Margin::new(0, 1)), &mut sb);
    }
}

/// `rack@host` -> `rack` when there is only one server to tell apart.
fn short_label<'a>(app: &crate::app::App, label: &'a str) -> &'a str {
    match app.servers.as_slice() {
        [one] => label.strip_suffix(&format!("@{}", one.server)).unwrap_or(label),
        _ => label,
    }
}
