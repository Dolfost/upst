//! Number, unit and duration formatting shared by the UI and --once.

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local};

/// Compact duration: `5d 13h`, `2h 05m`, `12m 30s`, `45s`.
pub fn duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "-".into();
    }
    let s = secs.round() as u64;
    let (d, h, m, s) = (s / 86400, s / 3600 % 24, s / 60 % 60, s % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Like `duration` but one unit finer for long spans: `5d 13h 18m`.
pub fn duration_long(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "-".into();
    }
    let s = secs.round() as u64;
    if s >= 86400 { format!("{}d {}h {:02}m", s / 86400, s / 3600 % 24, s / 60 % 60) } else { duration(secs) }
}

/// A past moment: `08:42` today, `Sun 08:42` this week, else the date.
pub fn when(t: SystemTime) -> String {
    let at: DateTime<Local> = t.into();
    let now = Local::now();
    if at.date_naive() == now.date_naive() {
        at.format("%H:%M").to_string()
    } else if (now - at).num_days() < 6 {
        at.format("%a %H:%M").to_string()
    } else {
        at.format("%Y-%m-%d %H:%M").to_string()
    }
}

pub fn ago(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 1.5 { "just now".into() } else { format!("{} ago", duration(s)) }
}

/// Window lengths for charts: `30s`, `5m`, `1h`, `6h`.
pub fn span(secs: f64) -> String {
    let s = secs.round() as u64;
    let (d, h, m) = (s / 86400, s / 3600 % 24, s / 60 % 60);
    if s >= 86400 && s.is_multiple_of(3600) {
        if h == 0 { format!("{d}d") } else { format!("{d}d {h}h") }
    } else if (3600..86400).contains(&s) && s.is_multiple_of(60) {
        if m == 0 { format!("{h}h") } else { format!("{h}h {m}m") }
    } else if (60..3600).contains(&s) && s.is_multiple_of(60) {
        format!("{m}m")
    } else {
        duration(secs)
    }
}

/// Unit for a NUT variable, from its name.
pub fn unit(var: &str) -> &'static str {
    let last = var.rsplit('.').next().unwrap_or(var);
    let has = |part: &str| var.split('.').any(|p| p == part);
    if has("voltage") {
        "V"
    } else if has("current") {
        "A"
    } else if has("frequency") {
        "Hz"
    } else if has("temperature") {
        "C"
    } else if has("realpower") {
        "W"
    } else if has("power") {
        "VA"
    } else if has("humidity") || has("charge") || has("load") || has("efficiency") {
        "%"
    } else if has("runtime") || has("delay") || has("timer") || last == "uptime" {
        "s"
    } else if has("capacity") && var.starts_with("battery.") {
        "Ah"
    } else {
        ""
    }
}

/// A number with sensible precision: `231.4`, `13.0`, `100`.
pub fn num(v: f64) -> String {
    if !v.is_finite() {
        return "-".into();
    }
    if v.fract() == 0.0 && v.abs() < 1e9 {
        format!("{v:.0}")
    } else if v.abs() >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// Value with its unit, seconds rendered as a duration.
pub fn with_unit(var: &str, v: f64) -> String {
    match unit(var) {
        "s" => duration(v),
        "" => num(v),
        "%" => format!("{}%", num(v)),
        u => format!("{} {u}", num(v)),
    }
}

/// A raw NUT value with its unit; keeps the driver's own precision.
pub fn raw_with_unit(var: &str, raw: &str) -> String {
    let raw = raw.trim();
    match (unit(var), parse_num(raw)) {
        ("s", Some(v)) => duration_long(v),
        ("", _) | (_, None) => raw.to_string(),
        ("%", Some(_)) => format!("{raw}%"),
        (u, Some(_)) => format!("{raw} {u}"),
    }
}

/// Like `raw_with_unit` with short durations, for tight columns.
pub fn raw_short(var: &str, raw: &str) -> String {
    match (unit(var), parse_num(raw)) {
        ("s", Some(v)) => duration(v),
        _ => raw_with_unit(var, raw),
    }
}

/// Tolerant number parse: NUT values are strings and some drivers pad them.
pub fn parse_num(s: &str) -> Option<f64> {
    let v: f64 = s.trim().parse().ok()?;
    v.is_finite().then_some(v)
}

/// `"2"`, `"2s"`, `"500ms"`, `"5m"`, `"6h"`, `"1d"`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: f64 = n.parse().map_err(|_| format!("bad duration {s:?}"))?;
    let mul = match unit.trim() {
        "" | "s" | "sec" => 1.0,
        "ms" => 0.001,
        "m" | "min" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        u => return Err(format!("unknown unit {u:?} in {s:?} (use ms, s, m, h or d)")),
    };
    let secs = n * mul;
    if !secs.is_finite() || secs < 0.0 {
        return Err(format!("bad duration {s:?}"));
    }
    Ok(Duration::from_secs_f64(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(479880.0), "5d 13h");
        assert_eq!(duration_long(479880.0), "5d 13h 18m");
        assert_eq!(duration(3725.0), "1h 02m");
        assert_eq!(duration(65.0), "1m 05s");
        assert_eq!(duration(5.0), "5s");
        assert_eq!(span(900.0), "15m");
        assert_eq!(span(21600.0), "6h");
        assert_eq!(span(4500.0), "1h 15m");
        assert_eq!(span(2700.0), "45m");
        assert_eq!(span(479940.0), "5d 13h");
        assert_eq!(span(172800.0), "2d");
        assert_eq!(span(90000.0), "1d 1h");
    }

    #[test]
    fn units() {
        assert_eq!(unit("input.voltage"), "V");
        assert_eq!(unit("input.voltage.nominal"), "V");
        assert_eq!(unit("ups.realpower.nominal"), "W");
        assert_eq!(unit("ups.power"), "VA");
        assert_eq!(unit("battery.charge.low"), "%");
        assert_eq!(unit("battery.runtime"), "s");
        assert_eq!(unit("device.model"), "");
        assert_eq!(with_unit("battery.voltage", 13.0), "13 V");
        assert_eq!(with_unit("battery.voltage", 3.25), "3.25 V");
        assert_eq!(with_unit("input.voltage", 231.44), "231.4 V");
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("2").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("6h").unwrap(), Duration::from_secs(21600));
        assert!(parse_duration("5x").is_err());
        assert!(parse_duration("").is_err());
    }
}
