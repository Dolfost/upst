//! Typed events: what [`Ups::apply`](crate::Ups::apply),
//! [`Ups::lost`](crate::Ups::lost) and [`Ups::replay`](crate::Ups::replay)
//! report when the state of a UPS changes.
//!
//! Every event has a stable machine code ([`Event::code`]) to hang
//! notification rules on, a [`Severity`], the data that goes with it
//! ([`EventKind`]), and an English text for people ([`Display`](fmt::Display)),
//! which is what the upst dashboard shows.

use std::fmt;
use std::time::{Duration, SystemTime};

use crate::status::{Power, Severity};

/// Something that happened to a UPS, or to the connection to it.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Event {
    /// When it happened: the time of the poll (or journal record) that
    /// showed it.
    pub at: SystemTime,
    /// How much it matters.
    pub severity: Severity,
    /// What happened, with its details.
    pub kind: EventKind,
}

/// What happened. New kinds may be added in minor versions, so a `match`
/// needs a wildcard arm.
///
/// Status flags are the words of `ups.status`; see
/// [`flag_info`](crate::status::flag_info) for what each one means.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", rename_all = "snake_case"))]
pub enum EventKind {
    /// The first poll found the UPS already on battery (`OB`).
    StartedOnBattery,
    /// Utility power failed: `OB` appeared.
    PowerLost,
    /// Back on line power (`OL`) after running on battery.
    PowerRestored {
        /// How long the outage lasted.
        on_battery: Duration,
    },
    /// Off battery, but not back on line: to bypass, output off, or a
    /// status upst cannot place.
    LeftBattery {
        /// Where the load is fed from now.
        to: Power,
        /// How long the load ran on battery.
        on_battery: Duration,
    },
    /// `LB`: the battery is below its low threshold.
    BatteryLow {
        /// `battery.charge` in percent, when the UPS reports it.
        charge: Option<f64>,
    },
    /// `LB` went away.
    BatteryLowCleared,
    /// `RB`: the UPS asks for a battery replacement.
    ReplaceBattery,
    /// `RB` went away.
    ReplaceBatteryCleared,
    /// `OVER`: the load is more than the UPS can carry.
    Overload,
    /// `OVER` went away.
    OverloadCleared,
    /// `BYPASS`: the load bypasses the UPS and is unprotected.
    Bypass,
    /// `BYPASS` went away.
    BypassEnded,
    /// `OFF`: the UPS output is switched off.
    OutputOff,
    /// `OFF` went away.
    OutputOn,
    /// `FSD`: a forced shutdown is in progress.
    ForcedShutdown,
    /// `CAL` appeared (`started`) or went away.
    Calibration {
        /// True when calibration started, false when it ended.
        started: bool,
    },
    /// `ups.alarm` appeared or changed.
    Alarm {
        /// The new value of `ups.alarm`.
        text: String,
    },
    /// The `ALARM` status flag appeared (`set`) or went away.
    AlarmFlag {
        /// True when the flag appeared.
        set: bool,
    },
    /// `ups.test.result` appeared or changed.
    SelfTest {
        /// The new value of `ups.test.result`.
        result: String,
    },
    /// `NOCOMM`: the driver lost communication with the UPS.
    DriverLost,
    /// `NOCOMM` went away.
    DriverBack,
    /// `TRIM`: the UPS lowers a high input voltage.
    Trim,
    /// `BOOST`: the UPS raises a low input voltage.
    Boost,
    /// upsd answers but has no data for this UPS (`ERR DATA-STALE`,
    /// `ERR DRIVER-NOT-CONNECTED` and the like).
    DataStale {
        /// What upsd said.
        why: String,
    },
    /// Data for this UPS arrived again after [`EventKind::DataStale`].
    DataFresh {
        /// The reason the data was stale.
        was: String,
    },
    /// The connection to upsd broke after it had been working.
    UpsdLost {
        /// What went wrong.
        why: String,
    },
    /// upsd could not be reached before any data came from it.
    UpsdUnreachable {
        /// What went wrong.
        why: String,
    },
    /// Data came after upsd was lost ([`EventKind::UpsdLost`]) or could not
    /// be reached ([`EventKind::UpsdUnreachable`]), so also at the first
    /// contact after an unreachable start.
    UpsdBack {
        /// The upsd server, as in `localhost` or `nas:3500`.
        server: String,
    },
    /// Any other status flag appeared (`set`) or went away.
    Flag {
        /// The flag as `ups.status` has it, e.g. `HB` or `ECO`.
        flag: String,
        /// True when the flag appeared.
        set: bool,
    },
}

impl EventKind {
    /// The stable machine code of this kind of event, e.g. `power.lost`.
    ///
    /// Codes never change meaning between versions; new kinds get new
    /// codes. A kind with a yes/no field has one code per answer, e.g.
    /// `battery.calibration.started` and `battery.calibration.ended`.
    ///
    /// | Code | Kind |
    /// |---|---|
    /// | `power.started_on_battery` | [`StartedOnBattery`](EventKind::StartedOnBattery) |
    /// | `power.lost` | [`PowerLost`](EventKind::PowerLost) |
    /// | `power.restored` | [`PowerRestored`](EventKind::PowerRestored) |
    /// | `power.left_battery` | [`LeftBattery`](EventKind::LeftBattery) |
    /// | `power.bypass` | [`Bypass`](EventKind::Bypass) |
    /// | `power.bypass.ended` | [`BypassEnded`](EventKind::BypassEnded) |
    /// | `battery.low` | [`BatteryLow`](EventKind::BatteryLow) |
    /// | `battery.low.cleared` | [`BatteryLowCleared`](EventKind::BatteryLowCleared) |
    /// | `battery.replace` | [`ReplaceBattery`](EventKind::ReplaceBattery) |
    /// | `battery.replace.cleared` | [`ReplaceBatteryCleared`](EventKind::ReplaceBatteryCleared) |
    /// | `battery.calibration.started` | [`Calibration`](EventKind::Calibration) `{ started: true }` |
    /// | `battery.calibration.ended` | [`Calibration`](EventKind::Calibration) `{ started: false }` |
    /// | `load.overload` | [`Overload`](EventKind::Overload) |
    /// | `load.overload.cleared` | [`OverloadCleared`](EventKind::OverloadCleared) |
    /// | `output.off` | [`OutputOff`](EventKind::OutputOff) |
    /// | `output.on` | [`OutputOn`](EventKind::OutputOn) |
    /// | `input.trim` | [`Trim`](EventKind::Trim) |
    /// | `input.boost` | [`Boost`](EventKind::Boost) |
    /// | `ups.forced_shutdown` | [`ForcedShutdown`](EventKind::ForcedShutdown) |
    /// | `ups.alarm` | [`Alarm`](EventKind::Alarm) |
    /// | `ups.alarm.raised` | [`AlarmFlag`](EventKind::AlarmFlag) `{ set: true }` |
    /// | `ups.alarm.cleared` | [`AlarmFlag`](EventKind::AlarmFlag) `{ set: false }` |
    /// | `ups.self_test` | [`SelfTest`](EventKind::SelfTest) |
    /// | `driver.lost` | [`DriverLost`](EventKind::DriverLost) |
    /// | `driver.back` | [`DriverBack`](EventKind::DriverBack) |
    /// | `data.stale` | [`DataStale`](EventKind::DataStale) |
    /// | `data.fresh` | [`DataFresh`](EventKind::DataFresh) |
    /// | `upsd.lost` | [`UpsdLost`](EventKind::UpsdLost) |
    /// | `upsd.unreachable` | [`UpsdUnreachable`](EventKind::UpsdUnreachable) |
    /// | `upsd.back` | [`UpsdBack`](EventKind::UpsdBack) |
    /// | `status.flag.set` | [`Flag`](EventKind::Flag) `{ set: true }` |
    /// | `status.flag.cleared` | [`Flag`](EventKind::Flag) `{ set: false }` |
    pub fn code(&self) -> &'static str {
        use EventKind::*;
        match self {
            StartedOnBattery => "power.started_on_battery",
            PowerLost => "power.lost",
            PowerRestored { .. } => "power.restored",
            LeftBattery { .. } => "power.left_battery",
            Bypass => "power.bypass",
            BypassEnded => "power.bypass.ended",
            BatteryLow { .. } => "battery.low",
            BatteryLowCleared => "battery.low.cleared",
            ReplaceBattery => "battery.replace",
            ReplaceBatteryCleared => "battery.replace.cleared",
            Calibration { started: true } => "battery.calibration.started",
            Calibration { started: false } => "battery.calibration.ended",
            Overload => "load.overload",
            OverloadCleared => "load.overload.cleared",
            OutputOff => "output.off",
            OutputOn => "output.on",
            Trim => "input.trim",
            Boost => "input.boost",
            ForcedShutdown => "ups.forced_shutdown",
            Alarm { .. } => "ups.alarm",
            AlarmFlag { set: true } => "ups.alarm.raised",
            AlarmFlag { set: false } => "ups.alarm.cleared",
            SelfTest { .. } => "ups.self_test",
            DriverLost => "driver.lost",
            DriverBack => "driver.back",
            DataStale { .. } => "data.stale",
            DataFresh { .. } => "data.fresh",
            UpsdLost { .. } => "upsd.lost",
            UpsdUnreachable { .. } => "upsd.unreachable",
            UpsdBack { .. } => "upsd.back",
            Flag { set: true, .. } => "status.flag.set",
            Flag { set: false, .. } => "status.flag.cleared",
        }
    }

    /// The severity events of this kind have. Only
    /// [`StartedOnBattery`](EventKind::StartedOnBattery) varies: it is
    /// critical when the battery is already low.
    pub(crate) fn severity(&self) -> Severity {
        use EventKind::*;
        use Severity::*;
        match self {
            PowerRestored { .. } | LeftBattery { .. } | BatteryLowCleared | ReplaceBatteryCleared => Good,
            OverloadCleared | BypassEnded | OutputOn | AlarmFlag { set: false } | DriverBack | DataFresh { .. } => Good,
            StartedOnBattery | PowerLost | ReplaceBattery | Bypass | OutputOff | DataStale { .. } => Warn,
            BatteryLow { .. } | Overload | ForcedShutdown | Alarm { .. } | AlarmFlag { set: true } => Crit,
            DriverLost | UpsdLost { .. } | UpsdUnreachable { .. } => Crit,
            Calibration { .. } | SelfTest { .. } | Trim | Boost | UpsdBack { .. } | Flag { .. } => Info,
        }
    }

    /// The event for a status flag that appeared (`set`) or went away;
    /// `None` for flags that flip often and are covered by power events.
    pub(crate) fn from_flag(flag: &str, set: bool) -> Option<EventKind> {
        use EventKind::*;
        Some(match (flag, set) {
            ("OB", true) => PowerLost,
            ("LB", true) => BatteryLow { charge: None },
            ("LB", false) => BatteryLowCleared,
            ("RB", true) => ReplaceBattery,
            ("RB", false) => ReplaceBatteryCleared,
            ("OVER", true) => Overload,
            ("OVER", false) => OverloadCleared,
            ("BYPASS", true) => Bypass,
            ("BYPASS", false) => BypassEnded,
            ("OFF", true) => OutputOff,
            ("OFF", false) => OutputOn,
            ("FSD", true) => ForcedShutdown,
            ("CAL", started) => Calibration { started },
            ("ALARM", set) => AlarmFlag { set },
            ("NOCOMM", true) => DriverLost,
            ("NOCOMM", false) => DriverBack,
            ("TRIM", true) => Trim,
            ("BOOST", true) => Boost,
            ("OL" | "OB" | "CHRG" | "DISCHRG" | "TRIM" | "BOOST" | "COMM" | "FSD" | "TEST", _) => return None,
            (flag, set) => Flag { flag: flag.to_string(), set },
        })
    }
}

impl Event {
    /// An event of `kind` at `at`, with the severity events of that kind
    /// have.
    pub fn new(at: SystemTime, kind: EventKind) -> Event {
        Event { at, severity: kind.severity(), kind }
    }

    /// The stable machine code, see [`EventKind::code`].
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use EventKind::*;
        let secs = |d: &Duration| crate::fmt::duration(d.as_secs_f64());
        match self {
            StartedOnBattery => f.write_str("Running on battery"),
            PowerLost => f.write_str("Power failure, running on battery"),
            PowerRestored { on_battery } => write!(f, "Power restored after {} on battery", secs(on_battery)),
            LeftBattery { on_battery, .. } => write!(f, "Left battery power after {} on battery", secs(on_battery)),
            BatteryLow { charge: Some(c) } => write!(f, "Battery low ({c}%)"),
            BatteryLow { charge: None } => f.write_str("Battery low"),
            BatteryLowCleared => f.write_str("Battery no longer low"),
            ReplaceBattery => f.write_str("UPS asks for a battery replacement"),
            ReplaceBatteryCleared => f.write_str("Battery replacement warning cleared"),
            Overload => f.write_str("UPS overloaded"),
            OverloadCleared => f.write_str("Overload cleared"),
            Bypass => f.write_str("UPS switched to bypass, the load is unprotected"),
            BypassEnded => f.write_str("Bypass ended"),
            OutputOff => f.write_str("UPS output switched off"),
            OutputOn => f.write_str("UPS output back on"),
            ForcedShutdown => f.write_str("Forced shutdown in progress"),
            Calibration { started: true } => f.write_str("Calibration started"),
            Calibration { started: false } => f.write_str("Calibration ended"),
            Alarm { text } => write!(f, "Alarm: {text}"),
            AlarmFlag { set: true } => f.write_str("UPS alarm raised"),
            AlarmFlag { set: false } => f.write_str("UPS alarm cleared"),
            SelfTest { result } => write!(f, "Self-test: {result}"),
            DriverLost => f.write_str("Driver lost communication with the UPS"),
            DriverBack => f.write_str("Driver talks to the UPS again"),
            Trim => f.write_str("Input voltage high, trimming"),
            Boost => f.write_str("Input voltage low, boosting"),
            DataStale { why } => write!(f, "No data: {why}"),
            DataFresh { was } => write!(f, "UPS data is fresh again (was: {was})"),
            UpsdLost { why } => write!(f, "Lost upsd: {why}"),
            UpsdUnreachable { why } => write!(f, "Cannot reach upsd: {why}"),
            UpsdBack { server } => write!(f, "Reconnected to {server}"),
            Flag { flag, set: true } => write!(f, "Status flag {flag} set"),
            Flag { flag, set: false } => write!(f, "Status flag {flag} cleared"),
        }
    }
}

/// The English text the upst dashboard shows, e.g. `Power restored after
/// 2m 00s on battery`. It may be reworded between versions; match on
/// [`Event::code`] or [`Event::kind`] instead.
impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}
