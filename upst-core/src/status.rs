//! Decoding of `ups.status`, a space-separated list of flags such as
//! `OL CHRG` or `OB DISCHRG LB`.

/// How much an event or a state matters, from good news to critical.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Severity {
    /// Good news, e.g. power came back.
    Good,
    /// Worth knowing, nothing to do.
    Info,
    /// Needs attention, e.g. running on battery.
    Warn,
    /// Needs attention now, e.g. low battery or overload.
    Crit,
}

/// Where the load is being fed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Power {
    /// Utility power (`OL`).
    Online,
    /// The battery (`OB`).
    Battery,
    /// Utility power through the bypass, unprotected (`BYPASS`).
    Bypass,
    /// Nothing: the output is off (`OFF`).
    Off,
    /// The status says none of the above.
    Unknown,
}

/// What a status flag means, see [`flag_info`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FlagInfo {
    /// A stable machine code, e.g. `on_battery` for `OB`, or `unknown`
    /// for a flag upst does not know.
    pub code: &'static str,
    /// A short label, e.g. `On battery`; empty for an unknown flag.
    pub label: &'static str,
    /// How much the flag matters while it is set.
    pub severity: Severity,
    /// A longer explanation.
    pub meaning: &'static str,
}

/// What one status flag (a word of `ups.status`) means.
///
/// The codes are stable:
///
/// | Flag | Code |
/// |---|---|
/// | `OL` | `online` |
/// | `OB` | `on_battery` |
/// | `LB` | `low_battery` |
/// | `HB` | `high_battery` |
/// | `RB` | `replace_battery` |
/// | `CHRG` | `charging` |
/// | `DISCHRG` | `discharging` |
/// | `BYPASS` | `bypass` |
/// | `CAL` | `calibrating` |
/// | `OFF` | `output_off` |
/// | `OVER` | `overload` |
/// | `TRIM` | `trim` |
/// | `BOOST` | `boost` |
/// | `FSD` | `forced_shutdown` |
/// | `ALARM` | `alarm` |
/// | `ECO` | `eco` |
/// | `TEST` | `test` |
/// | `COMM` | `comm` |
/// | `NOCOMM` | `no_comm` |
/// | anything else | `unknown` |
///
/// ```
/// use upst_core::status::{Severity, flag_info};
///
/// let lb = flag_info("LB");
/// assert_eq!((lb.code, lb.label, lb.severity), ("low_battery", "Low battery", Severity::Crit));
/// ```
pub fn flag_info(flag: &str) -> FlagInfo {
    use Severity::*;
    let (code, label, severity, meaning) = match flag {
        "OL" => ("online", "On line", Good, "the load is fed from utility power"),
        "OB" => ("on_battery", "On battery", Warn, "utility power failed, the load runs on battery"),
        "LB" => ("low_battery", "Low battery", Crit, "the battery is below the low threshold, shutdown is near"),
        "HB" => ("high_battery", "High battery", Info, "battery voltage is above normal"),
        "RB" => ("replace_battery", "Replace battery", Warn, "the UPS asks for a battery replacement"),
        "CHRG" => ("charging", "Charging", Good, "the battery is charging"),
        "DISCHRG" => ("discharging", "Discharging", Warn, "the battery is discharging"),
        "BYPASS" => ("bypass", "Bypass", Warn, "the load bypasses the UPS and is unprotected"),
        "CAL" => ("calibrating", "Calibrating", Info, "runtime calibration is in progress"),
        "OFF" => ("output_off", "Output off", Warn, "the UPS output is switched off"),
        "OVER" => ("overload", "Overload", Crit, "the load exceeds what the UPS can carry"),
        "TRIM" => ("trim", "Trimming", Info, "the UPS lowers a high input voltage"),
        "BOOST" => ("boost", "Boosting", Info, "the UPS raises a low input voltage"),
        "FSD" => ("forced_shutdown", "Forced shutdown", Crit, "a forced shutdown is in progress"),
        "ALARM" => ("alarm", "Alarm", Crit, "the UPS raised an alarm, see ups.alarm"),
        "ECO" => ("eco", "ECO mode", Info, "high-efficiency mode, the inverter is idle"),
        "TEST" => ("test", "Self-test", Info, "a self-test is running"),
        "COMM" => ("comm", "Communicating", Good, "the driver talks to the UPS"),
        "NOCOMM" => ("no_comm", "No comm", Crit, "the driver cannot talk to the UPS"),
        _ => ("unknown", "", Info, "flag not known to upst"),
    };
    FlagInfo { code, label, severity, meaning }
}

/// A parsed `ups.status`: its flags in the order the UPS gave them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Status {
    tokens: Vec<String>,
}

impl Status {
    /// Split a `ups.status` value into its flags.
    pub fn parse(s: &str) -> Status {
        Status { tokens: s.split_whitespace().map(str::to_string).collect() }
    }

    /// The flags, e.g. `["OB", "DISCHRG", "LB"]`.
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }

    /// Whether the status has this flag.
    pub fn has(&self, token: &str) -> bool {
        self.tokens.iter().any(|t| t == token)
    }

    /// Where the load is fed from. `OB` wins over `BYPASS`, then `OFF`,
    /// then `OL`.
    pub fn power(&self) -> Power {
        if self.has("OB") {
            Power::Battery
        } else if self.has("BYPASS") {
            Power::Bypass
        } else if self.has("OFF") {
            Power::Off
        } else if self.has("OL") {
            Power::Online
        } else {
            Power::Unknown
        }
    }

    /// On battery, or discharging.
    pub fn discharging(&self) -> bool {
        self.has("OB") || self.has("DISCHRG")
    }

    /// Charging, and not discharging.
    pub fn charging(&self) -> bool {
        self.has("CHRG") && !self.discharging()
    }

    /// One phrase for the whole state, most urgent first: `FORCED
    /// SHUTDOWN`, `LOW BATTERY`, `OVERLOAD`, `ON BATTERY`, `ON BYPASS`,
    /// `OUTPUT OFF`, `ON LINE`, `NO STATUS` or `UNKNOWN`.
    pub fn headline(&self) -> (&'static str, Severity) {
        use Severity::*;
        if self.has("FSD") {
            ("FORCED SHUTDOWN", Crit)
        } else if self.has("LB") && self.has("OB") {
            ("LOW BATTERY", Crit)
        } else if self.has("OVER") {
            ("OVERLOAD", Crit)
        } else if self.has("OB") {
            ("ON BATTERY", Warn)
        } else if self.has("BYPASS") {
            ("ON BYPASS", Warn)
        } else if self.has("OFF") {
            ("OUTPUT OFF", Warn)
        } else if self.has("OL") {
            ("ON LINE", Good)
        } else if self.tokens.is_empty() {
            ("NO STATUS", Info)
        } else {
            ("UNKNOWN", Info)
        }
    }

    /// The worst flag present, or the headline's severity if that is
    /// worse.
    pub fn severity(&self) -> Severity {
        let worst = self.tokens.iter().map(|t| flag_info(t).severity).max().unwrap_or(Severity::Info);
        worst.max(self.headline().1)
    }

    /// Flags worth showing next to the headline (the power source is
    /// already in the headline).
    pub fn extra_flags(&self) -> impl Iterator<Item = &str> {
        let (head, _) = self.headline();
        self.tokens.iter().map(String::as_str).filter(move |t| match *t {
            "OL" => false,
            "OB" => false,
            "LB" => head != "LOW BATTERY",
            "OVER" => head != "OVERLOAD",
            "BYPASS" => head != "ON BYPASS",
            "OFF" => head != "OUTPUT OFF",
            "FSD" => head != "FORCED SHUTDOWN",
            // Discharging on battery says nothing new.
            "DISCHRG" => !self.has("OB"),
            _ => true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headlines() {
        assert_eq!(Status::parse("OL CHRG").headline().0, "ON LINE");
        assert_eq!(Status::parse("OB DISCHRG").headline().0, "ON BATTERY");
        assert_eq!(Status::parse("OB DISCHRG LB").headline(), ("LOW BATTERY", Severity::Crit));
        assert_eq!(Status::parse("OL LB").headline().0, "ON LINE");
        assert_eq!(Status::parse("OL LB").severity(), Severity::Crit);
        assert_eq!(Status::parse("").headline().0, "NO STATUS");
    }

    #[test]
    fn flags_have_codes() {
        assert_eq!(flag_info("OB").code, "on_battery");
        assert_eq!(flag_info("NOCOMM").severity, Severity::Crit);
        let x = flag_info("XYZ");
        assert_eq!((x.code, x.label, x.severity), ("unknown", "", Severity::Info));
    }

    #[test]
    fn extra_flags_skip_the_obvious() {
        let s = Status::parse("OB DISCHRG LB RB");
        assert_eq!(s.extra_flags().collect::<Vec<_>>(), ["RB"]);
        let s = Status::parse("OL CHRG LB");
        assert_eq!(s.extra_flags().collect::<Vec<_>>(), ["CHRG", "LB"]);
    }
}
