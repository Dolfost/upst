//! Small text-based drawing helpers: bars with threshold marks, range
//! meters, sparklines and big digits. They return spans and lines so
//! cards can mix them freely with text.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::Theme;

/// Collapse per-cell (glyph, style) pairs into as few spans as possible.
fn spans(cells: Vec<(&'static str, Style)>) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut cur = String::new();
    let mut style = None;
    for (g, s) in cells {
        if style != Some(s) {
            if let Some(st) = style {
                out.push(Span::styled(std::mem::take(&mut cur), st));
            }
            style = Some(s);
        }
        cur.push_str(g);
    }
    if let Some(st) = style {
        out.push(Span::styled(cur, st));
    }
    out
}

/// Horizontal bar, `ratio` in 0..=1, with marks at threshold ratios.
pub fn bar(t: &Theme, width: usize, ratio: f64, fill: Style, marks: &[(f64, Style)]) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let ratio = if ratio.is_finite() { ratio.clamp(0.0, 1.0) } else { 0.0 };
    let eighths = (ratio * width as f64 * 8.0).round() as usize;
    let mut cells: Vec<(&'static str, Style)> = (0..width)
        .map(|i| match eighths.saturating_sub(i * 8).min(8) {
            8 => (t.full(), fill),
            0 => (t.empty(), t.dim()),
            n => (t.eighth(n), fill),
        })
        .collect();
    for &(r, style) in marks {
        if !(0.0..=1.0).contains(&r) {
            continue;
        }
        let i = ((r * width as f64).round() as usize).min(width - 1);
        // Inside the fill a mark is a block in its own color, outside a
        // tick; foreground only, so no background can bleed.
        let filled = cells[i].0 != t.empty();
        cells[i] = if filled && t.color { (t.full(), style) } else { (t.tick(), style) };
    }
    spans(cells)
}

/// A track from `lo` to `hi` with the acceptable zone highlighted and a
/// needle at `value`. Used for voltages against transfer points.
pub fn meter(
    t: &Theme,
    width: usize,
    value: f64,
    lo: f64,
    hi: f64,
    ok: (f64, f64),
    needle: Style,
) -> Vec<Span<'static>> {
    if width == 0 || hi <= lo {
        return Vec::new();
    }
    let pos = |v: f64| (((v - lo) / (hi - lo)) * (width - 1) as f64).round().clamp(0.0, (width - 1) as f64) as usize;
    let (a, b) = (pos(ok.0), pos(ok.1));
    let (thin, thick) = t.track();
    let zone = t.sev(crate::status::Severity::Good);
    let mut cells: Vec<(&'static str, Style)> =
        (0..width).map(|i| if i >= a && i <= b { (thick, zone) } else { (thin, t.dim()) }).collect();
    if value.is_finite() {
        cells[pos(value)] = (t.needle(), needle);
    }
    spans(cells)
}

/// Scale for a sparkline: data range, widened so noise does not look
/// like a cliff.
pub fn spark_range(values: &[Option<f64>], floor0: bool) -> (f64, f64) {
    let (mut lo, mut hi) =
        values.iter().flatten().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
    if !lo.is_finite() {
        return (0.0, 1.0);
    }
    if floor0 {
        lo = lo.min(0.0);
    }
    let min_span = (hi.abs().max(lo.abs()) * 0.05).max(0.5);
    if hi - lo < min_span {
        let mid = (hi + lo) / 2.0;
        lo = mid - min_span / 2.0;
        hi = mid + min_span / 2.0;
    }
    (lo, hi)
}

/// Sparkline of `height` rows, top row first.
pub fn sparkline(
    t: &Theme,
    values: &[Option<f64>],
    lo: f64,
    hi: f64,
    height: usize,
    style: Style,
) -> Vec<Line<'static>> {
    let height = height.max(1);
    let levels: Vec<Option<usize>> = values
        .iter()
        .map(|v| {
            v.map(|v| {
                let r = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                // Never fully empty, so a present value always shows.
                ((r * (height * 8) as f64).round() as usize).max(1)
            })
        })
        .collect();
    (0..height)
        .map(|row| {
            let base = (height - 1 - row) * 8;
            let cells = levels
                .iter()
                .map(|l| match l {
                    None if row == height - 1 => (if t.ascii { " " } else { "\u{2508}" }, t.dim()),
                    None => (" ", Style::new()),
                    Some(l) => (t.spark(l.saturating_sub(base).min(8)), style),
                })
                .collect();
            Line::from(spans(cells))
        })
        .collect()
}

/// 3x5 pixel glyphs, one string per pixel row.
fn glyph(c: char) -> Option<[&'static str; 5]> {
    Some(match c {
        '0' => ["###", "# #", "# #", "# #", "###"],
        '1' => [" # ", "## ", " # ", " # ", "###"],
        '2' => ["###", "  #", "###", "#  ", "###"],
        '3' => ["###", "  #", "###", "  #", "###"],
        '4' => ["# #", "# #", "###", "  #", "  #"],
        '5' => ["###", "#  ", "###", "  #", "###"],
        '6' => ["###", "#  ", "###", "# #", "###"],
        '7' => ["###", "  #", "  #", "  #", "  #"],
        '8' => ["###", "# #", "###", "# #", "###"],
        '9' => ["###", "# #", "###", "  #", "###"],
        '%' => ["# #", "  #", " # ", "#  ", "# #"],
        '.' => ["   ", "   ", "   ", "   ", " # "],
        '-' => ["   ", "   ", "###", "   ", "   "],
        ' ' => ["   ", "   ", "   ", "   ", "   "],
        _ => return None,
    })
}

/// Big text, 3 rows tall with half blocks, or 5 rows tall and double
/// width with full blocks when `large`. ASCII themes always get the
/// 5-row form drawn with `#`.
pub fn big_text(t: &Theme, text: &str, large: bool) -> Vec<String> {
    let glyphs: Vec<_> = text.chars().filter_map(glyph).collect();
    let pixel = |g: &[&str; 5], row: usize, col: usize| g[row].as_bytes()[col] == b'#';
    if large || t.ascii {
        let (on, off) = if t.ascii { ("#", " ") } else { ("\u{2588}\u{2588}", "  ") };
        (0..5)
            .map(|row| {
                glyphs
                    .iter()
                    .map(|g| (0..3).map(|c| if pixel(g, row, c) { on } else { off }).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(off)
            })
            .collect()
    } else {
        (0..3)
            .map(|row| {
                glyphs
                    .iter()
                    .map(|g| {
                        (0..3)
                            .map(|c| {
                                let top = pixel(g, row * 2, c);
                                let bottom = row * 2 + 1 < 5 && pixel(g, row * 2 + 1, c);
                                match (top, bottom) {
                                    (true, true) => '\u{2588}',
                                    (true, false) => '\u{2580}',
                                    (false, true) => '\u{2584}',
                                    (false, false) => ' ',
                                }
                            })
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }
}

/// `Label   value...` with the label padded to `w` columns.
pub fn kv(t: &Theme, label: &str, w: usize, value: Vec<Span<'static>>) -> Line<'static> {
    let mut v = vec![Span::styled(format!("{label:<w$}"), t.label())];
    v.extend(value);
    Line::from(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Theme = Theme { color: true, ascii: false };

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn bar_fills_by_eighths() {
        let s = text(&bar(&T, 4, 0.5, Style::new(), &[]));
        assert_eq!(s, "\u{2588}\u{2588}\u{2591}\u{2591}");
        let s = text(&bar(&T, 4, 0.5 + 1.0 / 32.0, Style::new(), &[]));
        assert_eq!(s.chars().nth(2), Some('\u{258f}'));
        let s = text(&bar(&T, 10, 0.0, Style::new(), &[(0.2, Style::new())]));
        assert_eq!(s.chars().nth(2), Some('\u{2502}'));
    }

    #[test]
    fn big_digits_have_fixed_height() {
        assert_eq!(big_text(&T, "100%", false).len(), 3);
        assert_eq!(big_text(&T, "100%", true).len(), 5);
        let w: Vec<usize> = big_text(&T, "42", false).iter().map(|l| l.chars().count()).collect();
        assert_eq!(w, [7, 7, 7]);
    }

    #[test]
    fn sparkline_scales() {
        let rows = sparkline(&T, &[Some(0.0), Some(1.0), None], 0.0, 1.0, 1, Style::new());
        let s: String = rows[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(s, "\u{2581}\u{2588}\u{2508}");
    }
}
