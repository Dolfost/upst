//! Popups: help, instant commands, confirmation, login and SET VAR.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::overview::wrap;
use super::{Ctx, Theme, centered, popup};
use crate::app::{Dialog, Hit, Input};
use crate::model::dangerous;
use crate::status::Severity;
use crate::worker::Action;

pub fn draw(f: &mut Frame, area: Rect, cx: &mut Ctx) {
    let app = cx.app;
    let t = cx.t;
    match app.dialog.as_ref() {
        Some(Dialog::Help) => help(f, area, &t),
        Some(Dialog::Commands { selected }) => commands(f, area, cx, *selected),
        Some(Dialog::Confirm { ups, action, desc, typed, error }) => {
            confirm(f, area, cx, *ups, action, desc.as_deref(), typed, error.as_deref())
        }
        Some(Dialog::Login { server, user, password, focus, .. }) => {
            login(f, area, cx, *server, user, password, *focus)
        }
        Some(Dialog::Edit { ups, var, input, selected, error }) => {
            edit(f, area, cx, *ups, var, input, *selected, error.as_deref())
        }
        None => {}
    }
}

fn keyline(t: &Theme, keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (k, what) in keys {
        spans.push(Span::styled(k.to_string(), t.accent()));
        spans.push(Span::styled(format!(" {what}   "), t.dim()));
    }
    Line::from(spans)
}

fn help(f: &mut Frame, area: Rect, t: &Theme) {
    let sections: &[(&str, &[(&str, &str)])] = &[
        (
            "Everywhere",
            &[
                ("1-4  tab", "switch view"),
                ("[  ]", "previous / next UPS"),
                ("r", "poll now"),
                ("p", "pause polling"),
                ("<  >", "poll faster / slower"),
                ("c", "instant commands"),
                ("?", "this help"),
                ("q  ctrl+c", "quit"),
            ],
        ),
        ("Overview", &[("b", "next charge style: battery, lcd, segments, blocks, bar")]),
        (
            "History",
            &[("arrows  hjkl", "pick a chart"), ("enter  z", "zoom the chart"), ("+  -", "shorter / longer window")],
        ),
        (
            "Variables",
            &[
                ("j k  pgup pgdn  g G", "move"),
                ("/", "filter by name or value"),
                ("enter  e", "change a writable variable"),
                ("y", "copy the value (OSC 52)"),
            ],
        ),
        ("Events", &[("j k  pgup pgdn", "scroll"), ("f", "all UPSes / this one")]),
        ("Mouse", &[("click", "tabs, UPSes, rows, charts"), ("wheel", "scroll, or zoom the History window")]),
    ];
    let kw = 20;
    let mut lines = Vec::new();
    for (i, (title, keys)) in sections.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(Span::styled(title.to_string(), t.title())));
        for (k, what) in keys.iter() {
            lines.push(Line::from(vec![Span::styled(format!("  {k:<kw$}"), t.accent()), Span::raw(what.to_string())]));
        }
    }
    let h = lines.len() as u16 + 2;
    let r = centered(area, 64, h);
    let inner = popup(f, r, t, "Keys", None);
    f.render_widget(Paragraph::new(lines), inner);
}

fn commands(f: &mut Frame, area: Rect, cx: &mut Ctx, selected: usize) {
    let t = cx.t;
    let Some(u) = cx.app.current() else { return };
    let w = area.width.saturating_sub(4).min(90);
    let h = (u.cmds.len() as u16 + 4).min(area.height.saturating_sub(2));
    let r = centered(area, w, h);
    let inner = popup(f, r, &t, &format!("Instant commands for {}", u.name), None);
    let list_h = inner.height.saturating_sub(2) as usize;
    let name_w = u.cmds.iter().map(|c| c.name.len()).max().unwrap_or(10) + 2;
    let start = selected.saturating_sub(list_h.saturating_sub(1));
    let mut lines = Vec::new();
    for (i, c) in u.cmds.iter().enumerate().skip(start).take(list_h) {
        let danger = dangerous(&c.name);
        let mark = if danger { "! " } else { "  " };
        let name_style = if danger { t.sev(Severity::Crit) } else { t.value() };
        let mut spans = vec![
            Span::styled(mark, t.sev(Severity::Crit)),
            Span::styled(format!("{:<name_w$}", c.name), name_style),
            Span::styled(c.desc.clone().unwrap_or_default(), t.dim()),
        ];
        if i == selected {
            spans = spans.into_iter().map(|s| s.patch_style(t.selected())).collect();
        }
        cx.hit(Rect::new(inner.x, inner.y + lines.len() as u16, inner.width, 1), Hit::DialogRow(i));
        lines.push(Line::from(spans));
    }
    while lines.len() < list_h {
        lines.push(Line::raw(""));
    }
    lines.push(Line::raw(""));
    lines.push(keyline(&t, &[("enter", "run"), ("esc", "close"), ("!", "can cut power, asks you to type it")]));
    f.render_widget(Paragraph::new(lines), inner);
}

#[allow(clippy::too_many_arguments)]
fn confirm(
    f: &mut Frame,
    area: Rect,
    cx: &Ctx,
    ups: usize,
    action: &Action,
    desc: Option<&str>,
    typed: &Input,
    error: Option<&str>,
) {
    let t = cx.t;
    let u = &cx.app.ups[ups];
    let danger = matches!(action, Action::Command { cmd, .. } if dangerous(cmd));
    let w = area.width.saturating_sub(4).min(72);
    let tw = w.saturating_sub(2) as usize;
    let mut lines = vec![Line::from(vec![
        Span::styled(action.describe(), t.value().patch(if danger { t.sev(Severity::Crit) } else { Style::new() })),
        Span::styled(format!(" on {}", u.label), t.dim()),
    ])];
    if let Some(d) = desc {
        lines.extend(wrap(d, tw, t.dim()));
    }
    lines.push(Line::raw(""));
    let mut cursor = None;
    if danger {
        lines.extend(wrap(
            "This can switch off the load or drain the battery. Type the command name to run it:",
            tw,
            t.sev(Severity::Warn),
        ));
        cursor = Some(lines.len());
        lines.push(Line::from(vec![Span::styled("> ", t.accent()), Span::raw(typed.text.clone())]));
        if let Some(e) = error {
            lines.push(Line::from(Span::styled(e.to_string(), t.sev(Severity::Crit))));
        }
        lines.push(Line::raw(""));
        lines.push(keyline(&t, &[("enter", "run"), ("esc", "cancel")]));
    } else {
        lines.push(keyline(&t, &[("y  enter", "run"), ("n  esc", "cancel")]));
    }
    let h = lines.len() as u16 + 2;
    let r = centered(area, w, h);
    let title = if danger { "Run a risky command?" } else { "Run command?" };
    let inner = popup(f, r, &t, title, Some(if danger { Severity::Crit } else { Severity::Info }));
    if let Some(row) = cursor {
        f.set_cursor_position(Position::new(inner.x + 2 + typed.cursor as u16, inner.y + row as u16));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn field(t: &Theme, label: &str, input: &Input, mask: bool, focused: bool, w: usize) -> Line<'static> {
    let shown = if mask { "*".repeat(input.text.chars().count()) } else { input.text.clone() };
    let pad = w.saturating_sub(shown.chars().count());
    let style = t.field(focused);
    Line::from(vec![
        Span::styled(format!("{label:<10}"), if focused { t.accent() } else { t.label() }),
        Span::styled(format!(" {shown}{} ", " ".repeat(pad)), style),
    ])
}

fn login(f: &mut Frame, area: Rect, cx: &Ctx, server: usize, user: &Input, password: &Input, focus: usize) {
    let t = cx.t;
    let w = area.width.saturating_sub(4).min(64);
    let tw = w.saturating_sub(2) as usize;
    let fw = tw.saturating_sub(13);
    let srv = &cx.app.servers[server];
    let mut lines = wrap(
        "Commands and SET need an upsd user with \"instcmds\" or \"actions = SET\" in upsd.users. \
         The login is kept in memory for this session only.",
        tw,
        t.dim(),
    );
    if srv.server.host == "127.0.0.1" && srv.version.as_deref().is_some_and(|v| v.contains("demo")) {
        lines.extend(wrap("Demo: any user works, the password \"wrong\" is refused.", tw, t.accent()));
    }
    lines.push(Line::raw(""));
    let row0 = lines.len();
    lines.push(field(&t, "User", user, false, focus == 0, fw));
    lines.push(field(&t, "Password", password, true, focus == 1, fw));
    lines.push(Line::raw(""));
    lines.push(keyline(&t, &[("enter", "next / log in"), ("tab", "switch"), ("esc", "cancel")]));
    let h = lines.len() as u16 + 2;
    let r = centered(area, w, h);
    let inner = popup(f, r, &t, &format!("Log in to {}", srv.server), Some(Severity::Info));
    let input = if focus == 0 { user } else { password };
    let x = inner.x + 11 + input.cursor as u16;
    f.set_cursor_position(Position::new(x.min(inner.x + inner.width - 1), inner.y + (row0 + focus) as u16));
    f.render_widget(Paragraph::new(lines), inner);
}

#[allow(clippy::too_many_arguments)]
fn edit(
    f: &mut Frame,
    area: Rect,
    cx: &mut Ctx,
    ups: usize,
    var: &str,
    input: &Input,
    selected: usize,
    error: Option<&str>,
) {
    let t = cx.t;
    let u = &cx.app.ups[ups];
    let info = u.info.get(var);
    let w = area.width.saturating_sub(4).min(68);
    let tw = w.saturating_sub(2) as usize;
    let mut lines = vec![Line::from(vec![
        Span::styled(var.to_string(), t.value()),
        Span::styled(format!(" on {}", u.label), t.dim()),
    ])];
    if let Some(d) = info.and_then(|i| i.desc.as_deref()) {
        lines.extend(wrap(d, tw, t.dim()));
    }
    if let Some(r) = info.filter(|i| !i.ranges.is_empty()) {
        let r: Vec<String> = r.ranges.iter().map(|(a, b)| format!("{a}..{b}")).collect();
        lines.push(Line::from(Span::styled(format!("allowed: {}", r.join(", ")), t.dim())));
    }
    lines.push(Line::raw(""));
    let choices = info.map(|i| i.enum_values.clone()).unwrap_or_default();
    let mut cursor = None;
    let popup_top = lines.len();
    if choices.is_empty() {
        if info.is_none() {
            lines.push(Line::from(Span::styled("asking upsd for the type...", t.dim())));
        }
        cursor = Some(lines.len());
        lines.push(field(&t, "Value", input, false, true, tw.saturating_sub(13)));
    } else {
        let current = u.get(var).unwrap_or("");
        for (i, c) in choices.iter().enumerate() {
            let mark = if c == current { "* " } else { "  " };
            let style = if i == selected { t.selected() } else { Style::new() };
            lines.push(Line::from(vec![Span::styled(mark, t.accent()), Span::styled(format!(" {c} "), style)]));
        }
    }
    if let Some(e) = error {
        lines.push(Line::from(Span::styled(e.to_string(), t.sev(Severity::Crit))));
    }
    lines.push(Line::raw(""));
    lines.push(keyline(&t, &[("enter", "set"), ("esc", "cancel")]));
    let h = lines.len() as u16 + 2;
    let r = centered(area, w, h);
    let inner = popup(f, r, &t, "Change variable", Some(Severity::Info));
    if !choices.is_empty() {
        for i in 0..choices.len() {
            cx.hit(Rect::new(inner.x, inner.y + (popup_top + i) as u16, inner.width, 1), Hit::DialogRow(i));
        }
    }
    if let Some(row) = cursor {
        let x = inner.x + 11 + input.cursor as u16;
        f.set_cursor_position(Position::new(x.min(inner.x + inner.width - 1), inner.y + row as u16));
    }
    f.render_widget(Paragraph::new(lines), inner);
}
