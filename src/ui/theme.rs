//! Colors and glyphs. Only the 16 ANSI colors are used, and text on the
//! terminal's own background is never black or white, so light and dark
//! themes both work.

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::{Marker, border};
use ratatui::widgets::{Block, BorderType};

use crate::history::Band;
use crate::status::Severity;

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub color: bool,
    pub ascii: bool,
}

const ASCII_BORDER: border::Set = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

impl Theme {
    fn fg(&self, c: Color) -> Style {
        if self.color { Style::new().fg(c) } else { Style::new() }
    }

    pub fn sev_color(&self, s: Severity) -> Color {
        match s {
            Severity::Good => Color::Green,
            Severity::Info => Color::Cyan,
            Severity::Warn => Color::Yellow,
            Severity::Crit => Color::Red,
        }
    }

    pub fn sev(&self, s: Severity) -> Style {
        if self.color {
            Style::new().fg(self.sev_color(s))
        } else {
            match s {
                Severity::Good | Severity::Info => Style::new(),
                Severity::Warn => Style::new().add_modifier(Modifier::BOLD),
                Severity::Crit => Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            }
        }
    }

    /// Solid label for the headline state.
    pub fn badge(&self, s: Severity) -> Style {
        let base = Style::new().add_modifier(Modifier::BOLD);
        if self.color { base.fg(Color::Black).bg(self.sev_color(s)) } else { base.add_modifier(Modifier::REVERSED) }
    }

    pub fn accent(&self) -> Style {
        self.fg(Color::Cyan)
    }

    pub fn dim(&self) -> Style {
        if self.color { Style::new().fg(Color::DarkGray) } else { Style::new().add_modifier(Modifier::DIM) }
    }

    pub fn label(&self) -> Style {
        self.dim()
    }

    pub fn value(&self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    pub fn title(&self) -> Style {
        let s = Style::new().add_modifier(Modifier::BOLD);
        if self.color { s.fg(Color::Reset) } else { s }
    }

    /// One solid highlight; it overrides the colors of everything under
    /// it, so a selected row reads as one bar.
    pub fn selected(&self) -> Style {
        if self.color {
            Style::new().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::new().add_modifier(Modifier::REVERSED)
        }
    }

    /// Text input fields.
    pub fn field(&self, focused: bool) -> Style {
        if focused { self.selected() } else { Style::new().add_modifier(Modifier::UNDERLINED) }
    }

    /// Values that changed a moment ago.
    pub fn fresh(&self) -> Style {
        self.fg(Color::Yellow).add_modifier(Modifier::BOLD)
    }

    pub fn rw(&self) -> Style {
        self.fg(Color::Magenta)
    }

    pub fn border(&self, focused: bool) -> Style {
        if focused { self.accent() } else { self.dim() }
    }

    pub fn block<'a>(&self, focused: bool) -> Block<'a> {
        let b = Block::bordered().border_style(self.border(focused)).title_style(self.title());
        if self.ascii { b.border_set(ASCII_BORDER) } else { b.border_type(BorderType::Rounded) }
    }

    /// Color for a variable's group prefix (`battery.`, `input.` ...).
    pub fn group(&self, prefix: &str) -> Style {
        let c = match prefix {
            "battery" => Color::Green,
            "ups" => Color::Cyan,
            "input" => Color::Yellow,
            "output" => Color::Magenta,
            "device" => Color::Blue,
            "ambient" | "outlet" => Color::LightBlue,
            "driver" | "server" => Color::DarkGray,
            _ => Color::Reset,
        };
        self.fg(c)
    }

    pub fn band(&self, b: Band) -> (&'static str, Style) {
        if !self.color || self.ascii {
            let ch = match b {
                Band::Online => "=",
                Band::Battery => "b",
                Band::Low => "L",
                Band::Other => "o",
                Band::NoData => " ",
            };
            return (ch, if b == Band::Online { self.dim() } else { self.value() });
        }
        let c = match b {
            Band::Online => Color::Green,
            Band::Battery => Color::Yellow,
            Band::Low => Color::Red,
            Band::Other => Color::Blue,
            Band::NoData => return ("\u{2500}", self.dim()),
        };
        ("\u{2588}", Style::new().fg(c))
    }

    pub fn marker(&self) -> Marker {
        if self.ascii { Marker::Custom('*') } else { Marker::Braille }
    }

    pub fn full(&self) -> &'static str {
        if self.ascii { "#" } else { "\u{2588}" }
    }

    pub fn empty(&self) -> &'static str {
        if self.ascii { "." } else { "\u{2591}" }
    }

    /// Left-aligned partial blocks, index = eighths filled (1..=7).
    pub fn eighth(&self, n: usize) -> &'static str {
        const E: [&str; 8] = [" ", "\u{258f}", "\u{258e}", "\u{258d}", "\u{258c}", "\u{258b}", "\u{258a}", "\u{2589}"];
        if self.ascii { if n >= 4 { "#" } else { "." } } else { E[n.min(7)] }
    }

    /// Bottom-aligned blocks for sparklines, index 0..=8.
    pub fn spark(&self, n: usize) -> &'static str {
        const S: [&str; 9] =
            [" ", "\u{2581}", "\u{2582}", "\u{2583}", "\u{2584}", "\u{2585}", "\u{2586}", "\u{2587}", "\u{2588}"];
        const A: [&str; 9] = [" ", ".", ".", "-", "-", "=", "=", "#", "#"];
        if self.ascii { A[n.min(8)] } else { S[n.min(8)] }
    }

    pub fn tick(&self) -> &'static str {
        if self.ascii { "|" } else { "\u{2502}" }
    }

    /// Stripe in front of event lines.
    pub fn stripe(&self) -> &'static str {
        if self.ascii { "|" } else { "\u{258c}" }
    }

    pub fn track(&self) -> (&'static str, &'static str) {
        if self.ascii { ("-", "=") } else { ("\u{2500}", "\u{2501}") }
    }

    pub fn needle(&self) -> &'static str {
        if self.ascii { "O" } else { "\u{2588}" }
    }
}

impl Theme {
    pub fn scrollbar(&self) -> ratatui::widgets::Scrollbar<'static> {
        let (thumb, track) = if self.ascii { ("#", "|") } else { ("\u{2503}", "\u{2502}") };
        ratatui::widgets::Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_symbol(thumb)
            .track_symbol(Some(track))
            .track_style(self.dim())
            .thumb_style(self.accent())
    }
}

impl Theme {
    /// Marker for threshold lines, distinct from the data.
    pub fn dashed(&self) -> Marker {
        Marker::Custom(if self.ascii { '-' } else { '\u{2504}' })
    }
}
