//! The battery charge in the style picked with `--charge-style` or `b`.
//! Every style fits on one line and grows into the rows the Battery card
//! can spare.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::Theme;
use super::widgets::{bar, big_text, kv};
use crate::cli::ChargeStyle;
use crate::fmt;
use crate::status::Severity;

pub struct Charge {
    /// Percent.
    pub value: f64,
    pub sev: Severity,
    pub low: Option<f64>,
    pub warn: Option<f64>,
}

impl Charge {
    fn text(&self) -> String {
        fmt::num(self.value.round())
    }

    /// Thresholds in position order, with their names and colors.
    fn marks(&self) -> Vec<(f64, &'static str, Severity)> {
        let mut m = Vec::new();
        if let Some(l) = self.low {
            m.push((l, "low", Severity::Crit));
        }
        if let Some(w) = self.warn {
            m.push((w, "warning", Severity::Warn));
        }
        m.retain(|(v, _, _)| (0.0..=100.0).contains(v));
        m.sort_by(|a, b| a.0.total_cmp(&b.0));
        m
    }
}

type Cell = (char, Style);

fn cells_line(cells: Vec<Cell>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur = String::new();
    let mut style = None;
    for (c, s) in cells {
        if style != Some(s) {
            if let Some(st) = style {
                spans.push(Span::styled(std::mem::take(&mut cur), st));
            }
            style = Some(s);
        }
        cur.push(c);
    }
    if let Some(st) = style {
        spans.push(Span::styled(cur, st));
    }
    Line::from(spans)
}

fn glyph(s: &str) -> char {
    s.chars().next().unwrap_or(' ')
}

/// Lines for the charge: at least one, at most `rows`. `lw` is the label
/// width of the card's other lines.
pub fn lines(t: &Theme, style: ChargeStyle, c: &Charge, w: usize, rows: usize, lw: usize) -> Vec<Line<'static>> {
    let has_marks = !c.marks().is_empty();
    let mut out = match style {
        ChargeStyle::Bar => vec![bar_line(t, c, w, lw)],
        ChargeStyle::Lcd | ChargeStyle::Blocks => digits_over_bar(t, style, c, w, rows, lw, has_marks),
        ChargeStyle::Battery => {
            let h = [7, 5, 3].into_iter().find(|h| *h <= rows && w >= if *h == 7 { 24 } else { 12 });
            match h {
                Some(h) => {
                    let mut v = gauge(t, c, w, h);
                    if rows > h && has_marks {
                        v.push(scale(t, c, w));
                    }
                    return v;
                }
                None => vec![bar_line(t, c, w, lw)],
            }
        }
        ChargeStyle::Segments => {
            // Segments as tall as the digits beside them: 5 rows with room
            // for the legend under them, else 3.
            for h in [5, 3] {
                let digits = seven_seg(t, &c.text(), h);
                let ladder = w.saturating_sub(digits[0].chars().count() + 4);
                if rows < h + usize::from(h == 5 && has_marks) || ladder < 9 {
                    continue;
                }
                let cells = segment_cells(t, c, ladder);
                let mut v: Vec<Line<'static>> = (0..h)
                    .map(|r| {
                        let mut l = cells_line(cells.clone());
                        l.spans.push(Span::raw("  "));
                        let pct = if r + 1 == h { " %" } else { "  " };
                        l.spans.push(Span::styled(format!("{}{pct}", digits[r]), t.sev(c.sev)));
                        l
                    })
                    .collect();
                if rows > h && has_marks {
                    v.extend(legend(t, c, w, 0));
                }
                return v;
            }
            vec![one_line(t, c, w, lw, |bw| cells_line(segment_cells(t, c, bw)).spans)]
        }
    };
    if out.len() < rows && has_marks {
        out.extend(legend(t, c, w, lw));
    }
    out
}

/// `Charge  [bar with marks]  49%`.
fn bar_line(t: &Theme, c: &Charge, w: usize, lw: usize) -> Line<'static> {
    let marks: Vec<(f64, Style)> = c.marks().iter().map(|(v, _, s)| (v / 100.0, t.sev(*s))).collect();
    one_line(t, c, w, lw, |bw| bar(t, bw, c.value / 100.0, t.sev(c.sev), &marks))
}

/// The label, a gauge `draw` makes for the width left, and the value; the
/// label goes first when the card is narrow.
fn one_line(t: &Theme, c: &Charge, w: usize, lw: usize, draw: impl Fn(usize) -> Vec<Span<'static>>) -> Line<'static> {
    let value = format!(" {:>4}", format!("{}%", c.text()));
    let style = t.value().patch(t.sev(c.sev));
    if w >= lw + value.len() + 4 {
        let mut spans = draw(w - lw - value.len());
        spans.push(Span::styled(value, style));
        return kv(t, "Charge", lw, spans);
    }
    let mut spans = draw(w.saturating_sub(value.len()));
    spans.push(Span::styled(value.trim_start().to_string(), style));
    Line::from(spans)
}

/// `low 40%  warning 55%` in the colors of their marks.
/// Under the bar when it fits, else from the left edge, else without
/// the names; nothing when not even that fits.
fn legend(t: &Theme, c: &Charge, w: usize, lw: usize) -> Option<Line<'static>> {
    let marks = c.marks();
    for (indent, named) in [(lw, true), (0, true), (0, false)] {
        let parts: Vec<(String, Severity)> = marks
            .iter()
            .map(|(v, name, sev)| {
                let v = fmt::num(*v);
                (if named { format!("{name} {v}%") } else { format!("{v}%") }, *sev)
            })
            .collect();
        let len = indent + parts.iter().map(|(p, _)| p.len()).sum::<usize>() + 2 * parts.len().saturating_sub(1);
        if len <= w {
            let mut spans = vec![Span::raw(" ".repeat(indent))];
            for (i, (p, sev)) in parts.into_iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(p, t.sev(sev)));
            }
            return Some(Line::from(spans));
        }
    }
    None
}

/// Big digits, a blank line and the bar; or just the bar when the rows
/// are not there.
fn digits_over_bar(
    t: &Theme,
    style: ChargeStyle,
    c: &Charge,
    w: usize,
    rows: usize,
    lw: usize,
    has_marks: bool,
) -> Vec<Line<'static>> {
    for large in [true, false] {
        let mut digits = if style == ChargeStyle::Lcd {
            seven_seg(t, &c.text(), if large { 5 } else { 3 })
        } else {
            big_text(t, &c.text(), large)
        };
        // A small percent sign on the baseline.
        let n = digits.len();
        for (i, r) in digits.iter_mut().enumerate() {
            r.push_str(if i + 1 == n { " %" } else { "  " });
        }
        // Large digits only with room left for the legend.
        let need = n + 2 + usize::from(large && has_marks);
        if need > rows || digits[0].chars().count() > w || (large && w < 30) {
            continue;
        }
        let mut out: Vec<Line<'static>> =
            digits.into_iter().map(|r| Line::from(Span::styled(r, t.sev(c.sev))).centered()).collect();
        out.push(Line::raw(""));
        out.push(bar_line(t, c, w, lw));
        return out;
    }
    vec![bar_line(t, c, w, lw)]
}

/// Bit per segment, a to g: top, top right, bottom right, bottom, bottom
/// left, top left, middle.
const SEGMENTS: [u8; 10] = [63, 6, 91, 79, 102, 109, 125, 7, 127, 111];

/// A box-drawing character joining lines in the given directions.
fn joint(t: &Theme, left: bool, right: bool, up: bool, down: bool) -> char {
    if t.ascii {
        return match (left || right, up || down) {
            (false, false) => ' ',
            (true, false) => '-',
            (false, true) => '|',
            (true, true) => '+',
        };
    }
    match (left, right, up, down) {
        (false, false, false, false) => ' ',
        (true, false, false, false) => '\u{2578}',
        (false, true, false, false) => '\u{257a}',
        (false, false, true, false) => '\u{2579}',
        (false, false, false, true) => '\u{257b}',
        (true, true, false, false) => '\u{2501}',
        (false, false, true, true) => '\u{2503}',
        (false, true, false, true) => '\u{250f}',
        (true, false, false, true) => '\u{2513}',
        (false, true, true, false) => '\u{2517}',
        (true, false, true, false) => '\u{251b}',
        (true, true, false, true) => '\u{2533}',
        (true, true, true, false) => '\u{253b}',
        (false, true, true, true) => '\u{2523}',
        (true, false, true, true) => '\u{252b}',
        (true, true, true, true) => '\u{254b}',
    }
}

/// One seven-segment digit, `w` columns by `h` rows (odd, at least 3).
fn seg_digit(t: &Theme, segs: u8, w: usize, h: usize) -> Vec<Vec<char>> {
    let on = |bit: u8| segs & (1 << bit) != 0;
    let (a, b, c, d, e, f, g) = (on(0), on(1), on(2), on(3), on(4), on(5), on(6));
    let k = h / 2;
    let mut rows = vec![vec![' '; w]; h];
    let horiz = joint(t, true, true, false, false);
    let vert = joint(t, false, false, true, true);
    for (r, bar, up_l, down_l, up_r, down_r) in
        [(0, a, false, f, false, b), (k, g, f, e, b, c), (h - 1, d, e, false, c, false)]
    {
        if bar {
            for cell in rows[r].iter_mut().take(w - 1).skip(1) {
                *cell = horiz;
            }
        }
        rows[r][0] = joint(t, false, bar, up_l, down_l);
        rows[r][w - 1] = joint(t, bar, false, up_r, down_r);
    }
    for (y, row) in rows.iter_mut().enumerate() {
        let (left, right) = if y > 0 && y < k {
            (f, b)
        } else if y > k && y < h - 1 {
            (e, c)
        } else {
            continue;
        };
        if left {
            row[0] = vert;
        }
        if right {
            row[w - 1] = vert;
        }
    }
    rows
}

/// Seven-segment digits like a UPS front panel, 3 or 5 rows tall.
pub fn seven_seg(t: &Theme, text: &str, h: usize) -> Vec<String> {
    let h = if h >= 5 { 5 } else { 3 };
    let w = if h == 5 { 5 } else { 3 };
    let digits: Vec<Vec<Vec<char>>> = text
        .chars()
        .filter_map(|ch| match ch {
            '0'..='9' => Some(SEGMENTS[ch as usize - '0' as usize]),
            '-' => Some(64),
            _ => None,
        })
        .map(|s| seg_digit(t, s, w, h))
        .collect();
    (0..h).map(|r| digits.iter().map(|d| d[r].iter().collect::<String>()).collect::<Vec<_>>().join(" ")).collect()
}

/// A lit cell on the fill: dark on the fill color.
fn on_fill(t: &Theme, fill: Color) -> Style {
    if t.color {
        Style::new().fg(Color::Black).bg(fill).add_modifier(Modifier::BOLD)
    } else {
        Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    }
}

/// A battery `h` rows tall (3, 5 or 7) filling `w` columns, with the
/// percentage inside and the thresholds marked on its edges.
fn gauge(t: &Theme, c: &Charge, w: usize, h: usize) -> Vec<Line<'static>> {
    let bw = w.saturating_sub(3);
    let body = h - 2;
    let fill = t.sev_color(c.sev);
    let fill_style = t.sev(c.sev);
    let border = t.dim();
    // Whole cells: a partial block would not line up with a filled
    // background.
    let full = ((c.value / 100.0).clamp(0.0, 1.0) * bw as f64).round() as usize;
    let marks: Vec<(usize, Severity)> = c
        .marks()
        .iter()
        .map(|(v, _, s)| (((v / 100.0) * bw as f64).round().min(bw as f64 - 1.0) as usize, *s))
        .collect();

    let (tl, tr, bl, br, hz, vt) = if t.ascii {
        ('+', '+', '+', '+', '-', '|')
    } else {
        ('\u{256d}', '\u{256e}', '\u{2570}', '\u{256f}', '\u{2500}', '\u{2502}')
    };
    let (tick_top, tick_bottom, tick_mid) =
        if t.ascii { ('+', '+', ':') } else { ('\u{252c}', '\u{2534}', '\u{2506}') };
    let edge = |top: bool| -> Vec<Cell> {
        let mut row = vec![(if top { tl } else { bl }, border)];
        row.extend((0..bw).map(|_| (hz, border)));
        row.push((if top { tr } else { br }, border));
        row.push((' ', Style::new()));
        for &(x, s) in &marks {
            row[1 + x] = (if top { tick_top } else { tick_bottom }, t.sev(s));
        }
        row
    };
    let mut grid: Vec<Vec<Cell>> = Vec::with_capacity(h);
    grid.push(edge(true));
    // The terminal on the right: a bracket over three rows, or a stub.
    let nub_rows: Vec<usize> = if body >= 3 { vec![h / 2 - 1, h / 2, h / 2 + 1] } else { vec![h / 2] };
    for y in 1..=body {
        let mut row = vec![(vt, border)];
        for x in 0..bw {
            let cell = if x >= full {
                (' ', Style::new())
            } else if t.color {
                (' ', Style::new().bg(fill))
            } else {
                (glyph(t.full()), fill_style)
            };
            row.push(cell);
        }
        for &(x, s) in &marks {
            if x >= full {
                row[1 + x] = (tick_mid, t.sev(s));
            }
        }
        let (side, nub) = match (nub_rows.iter().position(|r| *r == y), nub_rows.len(), t.ascii) {
            (None, _, _) => (vt, ' '),
            (Some(_), 1, true) => (vt, ']'),
            (Some(_), 1, false) => (vt, '\u{258c}'),
            (Some(1), _, _) => (vt, vt),
            (Some(_), _, true) => (vt, '+'),
            (Some(0), _, false) => ('\u{251c}', '\u{256e}'),
            (Some(_), _, false) => ('\u{251c}', '\u{256f}'),
        };
        row.push((side, border));
        row.push((nub, border));
        grid.push(row);
    }
    grid.push(edge(false));

    // The percentage: digits when the body is tall and wide enough.
    let text = c.text();
    let digits = match body {
        5 if seven_seg(t, &text, 5)[0].chars().count() + 4 <= bw => Some(seven_seg(t, &text, 5)),
        3 | 5 if seven_seg(t, &text, 3)[0].chars().count() + 4 <= bw => Some(seven_seg(t, &text, 3)),
        _ => None,
    };
    let overlay: Vec<Vec<char>> = match digits {
        Some(mut rows) => {
            let n = rows.len();
            for (i, r) in rows.iter_mut().enumerate() {
                r.push_str(if i + 1 == n { " %" } else { "  " });
            }
            rows.into_iter().map(|r| r.chars().collect()).collect()
        }
        None => vec![format!("{text}%").chars().collect()],
    };
    let oh = overlay.len().min(body);
    let ow = overlay.iter().map(Vec::len).max().unwrap_or(0).min(bw);
    // Centered in the filled or the empty part, whichever is longer, so
    // the number does not straddle the edge; centered on the body when
    // neither has room.
    let (from, to) = if full * 2 >= bw { (0, full) } else { (full, bw) };
    let left = if to - from >= ow + 2 { from + (to - from - ow) / 2 } else { (bw - ow) / 2 };
    let (y0, x0) = (1 + (body - oh) / 2, 1 + left);
    let lit = on_fill(t, fill);
    let plain = t.value().patch(fill_style);
    for (dy, chars) in overlay.iter().take(oh).enumerate() {
        for (dx, &ch) in chars.iter().take(ow).enumerate() {
            let cell = &mut grid[y0 + dy][x0 + dx];
            if ch != ' ' {
                *cell = (ch, if left + dx < full { lit } else { plain });
            } else if cell.0 == tick_mid {
                // No threshold line through the digits.
                *cell = (' ', Style::new());
            }
        }
    }
    grid.into_iter().map(cells_line).collect()
}

/// Threshold values under their ticks on the gauge's bottom edge, named
/// when there is room.
fn scale(t: &Theme, c: &Charge, w: usize) -> Line<'static> {
    let bw = w.saturating_sub(3);
    let mut cells: Vec<Cell> = vec![(' ', Style::new()); w];
    let mut free = 0;
    for (v, name, sev) in c.marks() {
        let x = 1 + ((v / 100.0) * bw as f64).round().min(bw as f64 - 1.0) as usize;
        let long = format!("{name} {}%", fmt::num(v));
        let short = format!("{}%", fmt::num(v));
        for label in [long, short] {
            let n = label.chars().count();
            let start = x.saturating_sub(n / 2).max(free);
            if start + n <= w {
                for (i, ch) in label.chars().enumerate() {
                    cells[start + i] = (ch, t.sev(sev));
                }
                free = start + n + 2;
                break;
            }
        }
    }
    cells_line(cells)
}

/// `n` segments across `width` columns, lit up to the charge and colored
/// by the zone they stand for.
fn segment_cells(t: &Theme, c: &Charge, width: usize) -> Vec<Cell> {
    let n = if width >= 19 {
        10
    } else if width >= 9 {
        5
    } else {
        return bar(t, width, c.value / 100.0, t.sev(c.sev), &[])
            .into_iter()
            .flat_map(|s| {
                let st = s.style;
                s.content.chars().map(move |ch| (ch, st)).collect::<Vec<_>>()
            })
            .collect();
    };
    let step = 100.0 / n as f64;
    let lit = if c.value <= 0.0 { 0 } else { (c.value / step).ceil() as usize };
    let base = (width + 1) / n - 1;
    let extra = (width + 1) - n * (base + 1);
    let mut cells = Vec::with_capacity(width);
    for i in 0..n {
        if i > 0 {
            cells.push((' ', Style::new()));
        }
        let top = (i + 1) as f64 * step;
        let zone = if c.low.is_some_and(|l| top <= l + 1e-9) {
            Severity::Crit
        } else if c.warn.is_some_and(|w| top <= w + 1e-9) {
            Severity::Warn
        } else {
            Severity::Good
        };
        let cell = if i < lit { (glyph(t.full()), t.sev(zone)) } else { (glyph(t.empty()), t.dim()) };
        cells.extend(std::iter::repeat_n(cell, base + usize::from(i < extra)));
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Theme = Theme { color: true, ascii: false };

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    fn charge(v: f64) -> Charge {
        Charge { value: v, sev: Severity::Good, low: Some(40.0), warn: Some(55.0) }
    }

    #[test]
    fn seven_segment_digits() {
        let d = seven_seg(&T, "1078", 3);
        assert_eq!(
            d,
            [
                "  \u{257b} \u{250f}\u{2501}\u{2513} \u{257a}\u{2501}\u{2513} \u{250f}\u{2501}\u{2513}",
                "  \u{2503} \u{2503} \u{2503}   \u{2503} \u{2523}\u{2501}\u{252b}",
                "  \u{2579} \u{2517}\u{2501}\u{251b}   \u{2579} \u{2517}\u{2501}\u{251b}"
            ]
        );
        let a = seven_seg(&Theme { color: false, ascii: true }, "8", 5);
        assert_eq!(a, ["+---+", "|   |", "+---+", "|   |", "+---+"]);
    }

    #[test]
    fn every_style_fits_every_size() {
        for style in
            [ChargeStyle::Battery, ChargeStyle::Lcd, ChargeStyle::Segments, ChargeStyle::Blocks, ChargeStyle::Bar]
        {
            for w in 0..80 {
                for rows in 1..10 {
                    for v in [0.0, 3.0, 49.0, 100.0] {
                        let l = lines(&T, style, &charge(v), w, rows, 10);
                        assert!(!l.is_empty() && l.len() <= rows, "{style:?} {w}x{rows}: {}", l.len());
                        if w >= 5 {
                            for s in text(&l) {
                                assert!(s.chars().count() <= w, "{style:?} {w}x{rows} {v}: {s:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn gauge_shape() {
        let g = text(&gauge(&T, &charge(50.0), 23, 5));
        assert_eq!(g.len(), 5);
        assert!(g.iter().all(|r| r.chars().count() == 23), "{g:?}");
        // Ticks at 40 % and 55 % of the 20-column body.
        assert_eq!(g[0].chars().nth(9), Some('\u{252c}'));
        assert_eq!(g[0].chars().nth(12), Some('\u{252c}'));
        let s = text(&[scale(&T, &charge(50.0), 23)]);
        assert!(s[0].contains("40%") && s[0].contains("55%"), "{s:?}");
    }

    #[test]
    fn segments_light_up_by_zone() {
        let c = segment_cells(&T, &charge(41.0), 29);
        assert_eq!(c.len(), 29);
        let lit: Vec<Style> = c.iter().filter(|(ch, _)| *ch == '\u{2588}').map(|(_, s)| *s).collect();
        // Five segments of two cells: four red, one yellow.
        assert_eq!(lit.len(), 10);
        assert_eq!(lit[0], T.sev(Severity::Crit));
        assert_eq!(lit[9], T.sev(Severity::Warn));
    }
}
