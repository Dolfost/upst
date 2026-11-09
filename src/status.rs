//! Decoding of `ups.status`, a space-separated list of flags such as
//! `OL CHRG` or `OB DISCHRG LB`.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Good,
    Info,
    Warn,
    Crit,
}

/// Where the load is being fed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Power {
    Online,
    Battery,
    Bypass,
    Off,
    Unknown,
}

/// (short label, severity, long explanation) for one status flag.
pub fn flag_info(token: &str) -> (&'static str, Severity, &'static str) {
    use Severity::*;
    match token {
        "OL" => ("On line", Good, "the load is fed from utility power"),
        "OB" => ("On battery", Warn, "utility power failed, the load runs on battery"),
        "LB" => ("Low battery", Crit, "the battery is below the low threshold, shutdown is near"),
        "HB" => ("High battery", Info, "battery voltage is above normal"),
        "RB" => ("Replace battery", Warn, "the UPS asks for a battery replacement"),
        "CHRG" => ("Charging", Good, "the battery is charging"),
        "DISCHRG" => ("Discharging", Warn, "the battery is discharging"),
        "BYPASS" => ("Bypass", Warn, "the load bypasses the UPS and is unprotected"),
        "CAL" => ("Calibrating", Info, "runtime calibration is in progress"),
        "OFF" => ("Output off", Warn, "the UPS output is switched off"),
        "OVER" => ("Overload", Crit, "the load exceeds what the UPS can carry"),
        "TRIM" => ("Trimming", Info, "the UPS lowers a high input voltage"),
        "BOOST" => ("Boosting", Info, "the UPS raises a low input voltage"),
        "FSD" => ("Forced shutdown", Crit, "a forced shutdown is in progress"),
        "ALARM" => ("Alarm", Crit, "the UPS raised an alarm, see ups.alarm"),
        "ECO" => ("ECO mode", Info, "high-efficiency mode, the inverter is idle"),
        "TEST" => ("Self-test", Info, "a self-test is running"),
        "COMM" => ("Communicating", Good, "the driver talks to the UPS"),
        "NOCOMM" => ("No comm", Crit, "the driver cannot talk to the UPS"),
        _ => ("", Info, "flag not known to upst"),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    tokens: Vec<String>,
}

impl Status {
    pub fn parse(s: &str) -> Status {
        Status { tokens: s.split_whitespace().map(str::to_string).collect() }
    }

    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }

    pub fn has(&self, token: &str) -> bool {
        self.tokens.iter().any(|t| t == token)
    }

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

    pub fn discharging(&self) -> bool {
        self.has("OB") || self.has("DISCHRG")
    }

    pub fn charging(&self) -> bool {
        self.has("CHRG") && !self.discharging()
    }

    /// One phrase for the whole state, most urgent first.
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

    /// The worst flag present.
    pub fn severity(&self) -> Severity {
        let worst = self.tokens.iter().map(|t| flag_info(t).1).max().unwrap_or(Severity::Info);
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

/// Event text for a flag that appeared (`true`) or went away.
pub fn flag_event(token: &str, set: bool) -> Option<(Severity, String)> {
    use Severity::*;
    let ev = match (token, set) {
        ("OB", true) => (Warn, "Power failure, running on battery".to_string()),
        ("LB", true) => (Crit, "Battery low".to_string()),
        ("LB", false) => (Good, "Battery no longer low".to_string()),
        ("RB", true) => (Warn, "UPS asks for a battery replacement".to_string()),
        ("RB", false) => (Good, "Battery replacement warning cleared".to_string()),
        ("OVER", true) => (Crit, "UPS overloaded".to_string()),
        ("OVER", false) => (Good, "Overload cleared".to_string()),
        ("BYPASS", true) => (Warn, "UPS switched to bypass, the load is unprotected".to_string()),
        ("BYPASS", false) => (Good, "Bypass ended".to_string()),
        ("OFF", true) => (Warn, "UPS output switched off".to_string()),
        ("OFF", false) => (Good, "UPS output back on".to_string()),
        ("FSD", true) => (Crit, "Forced shutdown in progress".to_string()),
        ("CAL", true) => (Info, "Calibration started".to_string()),
        ("CAL", false) => (Info, "Calibration ended".to_string()),
        ("ALARM", true) => (Crit, "UPS alarm raised".to_string()),
        ("ALARM", false) => (Good, "UPS alarm cleared".to_string()),
        ("NOCOMM", true) => (Crit, "Driver lost communication with the UPS".to_string()),
        ("NOCOMM", false) => (Good, "Driver talks to the UPS again".to_string()),
        ("TRIM", true) => (Info, "Input voltage high, trimming".to_string()),
        ("BOOST", true) => (Info, "Input voltage low, boosting".to_string()),
        // These flip often and the power events already cover them.
        ("OL" | "OB" | "CHRG" | "DISCHRG" | "TRIM" | "BOOST" | "COMM" | "FSD" | "TEST", _) => {
            return None;
        }
        (t, true) => (Info, format!("Status flag {t} set")),
        (t, false) => (Info, format!("Status flag {t} cleared")),
    };
    Some(ev)
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
    fn extra_flags_skip_the_obvious() {
        let s = Status::parse("OB DISCHRG LB RB");
        assert_eq!(s.extra_flags().collect::<Vec<_>>(), ["RB"]);
        let s = Status::parse("OL CHRG LB");
        assert_eq!(s.extra_flags().collect::<Vec<_>>(), ["CHRG", "LB"]);
    }
}
