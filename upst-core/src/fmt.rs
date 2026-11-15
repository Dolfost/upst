//! Helpers for NUT variable values: their units and numbers.

/// Unit for a NUT variable, from its name: `V`, `A`, `Hz`, `C`, `W`,
/// `VA`, `%`, `s`, `Ah`, or an empty string when the name says nothing.
///
/// ```
/// use upst_core::fmt::unit;
/// assert_eq!(unit("input.voltage.nominal"), "V");
/// assert_eq!(unit("ups.realpower"), "W");
/// assert_eq!(unit("battery.runtime"), "s");
/// assert_eq!(unit("device.model"), "");
/// ```
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

/// Tolerant number parse: NUT values are strings and some drivers pad
/// them. Infinities and NaN count as no number.
///
/// ```
/// use upst_core::fmt::parse_num;
/// assert_eq!(parse_num(" 231.4 "), Some(231.4));
/// assert_eq!(parse_num("OL"), None);
/// ```
pub fn parse_num(s: &str) -> Option<f64> {
    let v: f64 = s.trim().parse().ok()?;
    v.is_finite().then_some(v)
}

/// Compact duration: `5d 13h`, `2h 05m`, `12m 30s`, `45s`, or `-` for a
/// negative or infinite number of seconds. This is what event texts use.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(479880.0), "5d 13h");
        assert_eq!(duration(3725.0), "1h 02m");
        assert_eq!(duration(65.0), "1m 05s");
        assert_eq!(duration(5.0), "5s");
        assert_eq!(duration(-1.0), "-");
        assert_eq!(duration(f64::NAN), "-");
    }

    #[test]
    fn units() {
        assert_eq!(unit("input.voltage"), "V");
        assert_eq!(unit("input.voltage.nominal"), "V");
        assert_eq!(unit("ups.realpower.nominal"), "W");
        assert_eq!(unit("ups.power"), "VA");
        assert_eq!(unit("battery.charge.low"), "%");
        assert_eq!(unit("battery.runtime"), "s");
        assert_eq!(unit("battery.capacity"), "Ah");
        assert_eq!(unit("device.model"), "");
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_num("13.0"), Some(13.0));
        assert_eq!(parse_num(" -5 "), Some(-5.0));
        assert_eq!(parse_num(""), None);
        assert_eq!(parse_num("inf"), None);
        assert_eq!(parse_num("NaN"), None);
    }
}
