//! Drawing. Every frame is rebuilt from `App`; layouts adapt to the
//! terminal size on each draw, so resizing needs no special handling.

mod charge;
mod dialog;
mod events;
mod history;
mod overview;
mod theme;
mod variables;
pub mod widgets;

use std::time::Instant;

use chrono::Local;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

pub use theme::Theme;

use crate::app::{App, Hit, ServerLink, View};
use crate::model::{Link, Ups};
use crate::status::Severity;

/// What the drawing code needs besides the frame.
pub struct Ctx<'a> {
    pub app: &'a App,
    pub t: Theme,
    pub hits: Vec<(Rect, Hit)>,
}

impl Ctx<'_> {
    pub fn hit(&mut self, r: Rect, h: Hit) {
        self.hits.push((r, h));
    }
}

/// Overall severity of a UPS, connection problems included.
pub fn ups_severity(u: &Ups) -> Severity {
    match u.link {
        Link::Up => u.status.severity(),
        Link::Connecting => Severity::Info,
        Link::Stale(_) => Severity::Warn,
        Link::Down(_) => Severity::Crit,
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let t = app.theme;
    if area.width < 24 || area.height < 6 {
        let msg = Paragraph::new("upst: terminal too small").style(t.dim()).wrap(Wrap { trim: true });
        f.render_widget(msg, area);
        return;
    }
    let mut table = std::mem::take(&mut app.vars.table);
    let mut cx = Ctx { app, t, hits: Vec::new() };

    let multi = cx.app.ups.len() > 1;
    let [header, strip, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(if multi { 1 } else { 0 }),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_header(f, header, &mut cx);
    if multi {
        draw_strip(f, strip, &mut cx);
    }
    let mut charts = None;
    match cx.app.current() {
        None => draw_waiting(f, body, &cx),
        Some(u) => match cx.app.view {
            View::Overview => overview::draw(f, body, &mut cx, u),
            View::History => charts = Some(history::draw(f, body, &mut cx, u)),
            View::Variables => variables::draw(f, body, &mut cx, u, &mut table),
            View::Events => events::draw(f, body, &mut cx),
        },
    }
    draw_footer(f, footer, &cx);
    if cx.app.dialog.is_some() {
        dialog::draw(f, area, &mut cx);
    }

    let hits = cx.hits;
    app.hits = hits;
    app.vars.table = table;
    if let Some(n) = charts {
        app.hist.count = n;
        if app.hist.selected >= n {
            app.hist.selected = n.saturating_sub(1);
        }
    }
}

fn draw_header(f: &mut Frame, area: Rect, cx: &mut Ctx) {
    let t = cx.t;
    let app = cx.app;
    let clock = Local::now().format("%H:%M:%S").to_string();
    let right = match (app.ups.len(), app.current()) {
        (1, Some(u)) => format!("{}  {clock} ", u.label),
        _ => format!("{clock} "),
    };
    let brand = Span::styled(" upst ", t.accent().add_modifier(ratatui::style::Modifier::BOLD));
    let mut spans = vec![brand, Span::raw(" ")];
    let mut x = area.x + 7;
    let room = area.width.saturating_sub(7 + right.len() as u16);
    let full: u16 = View::ALL.iter().map(|v| v.title().len() as u16 + 4).sum();
    for (i, v) in View::ALL.iter().enumerate() {
        let name = if room >= full {
            format!(" {} {} ", i + 1, v.title())
        } else if room >= 4 * 8 {
            format!(" {} {} ", i + 1, v.short())
        } else {
            format!(" {} ", i + 1)
        };
        let w = name.len() as u16;
        let style = if *v == app.view { t.selected().add_modifier(ratatui::style::Modifier::BOLD) } else { t.dim() };
        cx.hits.push((Rect::new(x, area.y, w, 1), Hit::Tab(*v)));
        spans.push(Span::styled(name, style));
        x += w;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    f.render_widget(Paragraph::new(Span::styled(right, t.dim())).alignment(Alignment::Right), area);
}

/// Short display name; the server only matters when there are several.
pub fn display_name(app: &App, u: &Ups) -> String {
    if app.servers.len() > 1 { u.label.clone() } else { u.name.clone() }
}

fn draw_strip(f: &mut Frame, area: Rect, cx: &mut Ctx) {
    let t = cx.t;
    let app = cx.app;
    let items: Vec<(usize, String, Severity)> =
        app.ups.iter().enumerate().map(|(i, u)| (i, display_name(app, u), ups_severity(u))).collect();
    let widths: Vec<u16> = items.iter().map(|(_, n, _)| n.chars().count() as u16 + 3).collect();
    // Scroll so the selected one is visible.
    let mut start = 0;
    while start < app.sel && widths[start..=app.sel].iter().sum::<u16>() > area.width {
        start += 1;
    }
    let mut spans = Vec::new();
    let mut x = area.x;
    for (i, name, sev) in &items[start..] {
        let w = widths[*i];
        if x + w > area.x + area.width {
            break;
        }
        let sel = *i == app.sel;
        let name_style = if sel { t.selected().add_modifier(ratatui::style::Modifier::BOLD) } else { Style::new() };
        spans.push(Span::styled(t.stripe(), t.sev(*sev)));
        spans.push(Span::styled(format!(" {name} "), name_style));
        spans.push(Span::raw(" "));
        cx.hits.push((Rect::new(x, area.y, w, 1), Hit::Ups(*i)));
        x += w;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    if app.servers.len() == 1 {
        let label = format!("{} ", app.servers[0].server);
        let used = x - area.x;
        if used + label.len() as u16 + 2 <= area.width {
            f.render_widget(Paragraph::new(Span::styled(label, t.dim())).alignment(Alignment::Right), area);
        }
    }
}

fn draw_waiting(f: &mut Frame, area: Rect, cx: &Ctx) {
    let t = cx.t;
    let mut lines = vec![Line::from(Span::styled("Waiting for upsd", t.title())), Line::raw("")];
    for s in &cx.app.servers {
        let (text, sev) = match &s.link {
            ServerLink::Connecting => (format!("{}: connecting", s.server), Severity::Info),
            ServerLink::Up => (format!("{}: connected, no UPS listed", s.server), Severity::Warn),
            ServerLink::Down { error, retry_at } => {
                let left = retry_at.saturating_duration_since(Instant::now()).as_secs();
                (format!("{}: {error}, retrying in {left}s", s.server), Severity::Crit)
            }
        };
        lines.push(Line::from(Span::styled(text, t.sev(sev))));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "Is upsd running and listening there? Try upst --demo to see upst work.",
        t.dim(),
    )));
    let h = lines.len() as u16 + 2;
    let r = centered(area, area.width.min(76), h.min(area.height));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center).wrap(Wrap { trim: true }), r);
}

fn draw_footer(f: &mut Frame, area: Rect, cx: &Ctx) {
    let t = cx.t;
    let app = cx.app;
    // (key, what, priority); the highest number goes first when space runs out.
    let mut keys: Vec<(&str, &str, u8)> = Vec::new();
    match app.view {
        View::Variables if app.vars.filtering => keys.extend([("enter", "keep", 2), ("esc", "clear", 2)]),
        View::Variables => {
            keys.push(("/", "filter", 2));
            let rw = app.selected_var().zip(app.current()).is_some_and(|(v, u)| u.rw.contains(&v));
            if rw {
                keys.push(("e", "edit", 2));
            }
            keys.push(("y", "copy", 6));
        }
        View::History => keys.extend([("+/-", "window", 2), ("enter", "zoom", 3)]),
        View::Events => keys.push(("f", if app.events.all { "this ups" } else { "all" }, 2)),
        View::Overview => keys.push(("b", "style", 6)),
    }
    if app.current().is_some_and(|u| !u.cmds.is_empty()) {
        keys.push(("c", "commands", 4));
    }
    if app.ups.len() > 1 {
        keys.push(("[ ]", "ups", 3));
    }
    keys.extend([("tab", "view", 5), ("?", "help", 0), ("q", "quit", 1)]);
    let width = |ks: &[(&str, &str, u8)]| ks.iter().map(|(k, w, _)| k.len() + w.len() + 3).sum::<usize>();

    let mut right: Vec<Span> = Vec::new();
    if app.paused {
        right.push(Span::styled(" PAUSED ", t.badge(Severity::Warn)));
        right.push(Span::raw(" "));
    }
    if let Some(toast) = &app.toast {
        let room = (area.width as usize).saturating_sub(20).max(10);
        let text: String = if toast.text.chars().count() > room {
            format!("{}...", toast.text.chars().take(room.saturating_sub(3)).collect::<String>())
        } else {
            toast.text.clone()
        };
        right.push(Span::styled(format!("{text} "), t.sev(toast.severity)));
    }
    let right_w: usize = right.iter().map(|s| s.content.chars().count()).sum();

    let mut left: Vec<Span> = Vec::new();
    if app.view == View::Variables && app.vars.filtering {
        left.push(Span::styled(" /", t.accent()));
        left.push(Span::raw(app.vars.filter.text.clone()));
        left.push(Span::styled(if t.ascii { "_" } else { "\u{2581}" }, t.accent()));
        left.push(Span::raw("  "));
    }
    let left_w: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let budget = (area.width as usize).saturating_sub(right_w + left_w + 1);
    while width(&keys) > budget && !keys.is_empty() {
        let worst = keys.iter().enumerate().max_by_key(|(_, k)| k.2).map(|(i, _)| i).unwrap();
        keys.remove(worst);
    }
    for (k, what, _) in keys {
        left.push(Span::styled(format!(" {k}"), t.accent()));
        left.push(Span::styled(format!(" {what} "), t.dim()));
    }
    f.render_widget(Paragraph::new(Line::from(left)), area);
    if !right.is_empty() {
        f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), area);
    }
}

pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

/// Clear a popup area and draw its frame; returns the inner area.
pub fn popup(f: &mut Frame, area: Rect, t: &Theme, title: &str, severity: Option<Severity>) -> Rect {
    f.render_widget(Clear, area);
    let mut block = t.block(true).title(format!(" {title} "));
    if let Some(s) = severity {
        block = block.border_style(t.sev(s));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

#[cfg(test)]
pub mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::Options;
    use crate::model::CmdInfo;
    use crate::target::Server;
    use crate::worker::Update;

    /// An app fed with `steps` simulated polls, 2 s apart, ending now.
    pub fn demo_app(steps: usize, color: bool) -> App {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(Options {
            interval: Duration::from_secs(2),
            history: Duration::from_secs(6 * 3600),
            capacity: None,
            capacities: Vec::new(),
            bell: false,
            view: View::Overview,
            theme: Theme { color, ascii: false },
            charge_style: std::env::var("UPST_STYLE")
                .ok()
                .and_then(|s| clap::ValueEnum::from_str(&s, true).ok())
                .unwrap_or(crate::cli::ChargeStyle::Battery),
            user: None,
            password: None,
        });
        app.add_server(Server { host: "127.0.0.1".into(), port: 3493 }, tx, None);
        app.update(Update::Connected { server: 0, version: Some("upsd 2.8.4 (upst demo)".into()) });
        let now = SystemTime::now();
        for (name, desc, cmds, rw, frames) in crate::demo::record(steps, 2.0) {
            app.update(Update::UpsList { server: 0, list: vec![(name.clone(), desc)] });
            let cmds = cmds.into_iter().map(|(name, d)| CmdInfo { name, desc: Some(d) }).collect();
            app.update(Update::Meta { server: 0, ups: name.clone(), cmds, rw });
            let i = app.ups.iter().position(|u| u.name == name).unwrap();
            let n = frames.len();
            for (k, vars) in frames.into_iter().enumerate() {
                let at = now - Duration::from_secs_f64((n - 1 - k) as f64 * 2.0);
                let label = app.ups[i].label.clone();
                for (sev, text) in app.ups[i].apply(vars, at) {
                    app.log_event(&label, sev, text);
                }
            }
        }
        app
    }

    pub fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_at_every_size() {
        let mut app = demo_app(200, true);
        for view in View::ALL {
            app.view = view;
            for sel in 0..app.ups.len() {
                app.sel = sel;
                for w in (1..=220).step_by(7) {
                    for h in (1..=70).step_by(5) {
                        render(&mut app, w, h);
                    }
                }
            }
        }
    }

    /// `UPST_SNAP=120x40,80x24 UPST_STEPS=190 cargo test snapshots -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn snapshots() {
        let steps = std::env::var("UPST_STEPS").ok().and_then(|s| s.parse().ok()).unwrap_or(190);
        let sizes = std::env::var("UPST_SNAP").unwrap_or_else(|_| "120x40".into());
        let views = std::env::var("UPST_VIEWS").unwrap_or_else(|_| "1".into());
        let sel: usize = std::env::var("UPST_SEL").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        let mut app = demo_app(steps, true);
        app.sel = sel;
        app.theme.ascii = std::env::var_os("UPST_ASCII").is_some();
        if let Ok(d) = std::env::var("UPST_DIALOG") {
            app.dialog = Some(match d.as_str() {
                "help" => crate::app::Dialog::Help,
                _ => crate::app::Dialog::Commands { selected: 2 },
            });
        }
        for v in views.chars().filter_map(|c| c.to_digit(10)) {
            app.view = View::ALL[v as usize - 1];
            for size in sizes.split(',') {
                let (w, h) = size.split_once('x').unwrap();
                let (w, h) = (w.parse().unwrap(), h.parse().unwrap());
                println!("=== {} {w}x{h} ===\n{}", app.view.title(), render(&mut app, w, h));
            }
        }
    }
}
