//! Typed events: every transition `Ups::apply` and `Ups::lost` report.
//!
//! The expected severities and texts are those of upst 0.1, which made
//! events as (Severity, String) pairs (`status::flag_event` and
//! `Ups::apply` in the binary); the typed events must say the same.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use upst_core::status::flag_info;
use upst_core::{Event, EventKind, Link, Power, Severity, Ups};

use EventKind::*;
use Severity::*;

fn t0() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn at(secs: u64) -> SystemTime {
    t0() + Duration::from_secs(secs)
}

fn ups() -> Ups {
    Ups::builder("ups", "localhost").history(None).build()
}

fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn status(s: &str) -> Vec<(String, String)> {
    vars(&[("ups.status", s)])
}

fn down(why: &str) -> Link {
    Link::Down(why.into())
}

fn stale(why: &str) -> Link {
    Link::Stale(why.into())
}

/// `apply`, checking that every event carries the poll's time.
fn poll(u: &mut Ups, list: Vec<(String, String)>, at: SystemTime) -> Vec<Event> {
    let events = u.apply(list, at);
    assert!(events.iter().all(|e| e.at == at), "{events:?}");
    events
}

/// `lost`, checking that the event carries the given time.
fn lose(u: &mut Ups, link: Link, at: SystemTime) -> Option<Event> {
    let event = u.lost(link, at);
    assert!(event.iter().all(|e| e.at == at), "{event:?}");
    event
}

/// An event as (kind, code, severity, text).
type Seen = (EventKind, &'static str, Severity, String);

/// What we expect: (kind, code, severity, text).
type Expect<'a> = (EventKind, &'static str, Severity, &'a str);

fn seen(events: &[Event]) -> Vec<Seen> {
    events
        .iter()
        .map(|e| {
            assert_eq!(e.code(), e.kind.code());
            assert_eq!(e.to_string(), e.kind.to_string());
            (e.kind.clone(), e.code(), e.severity, e.to_string())
        })
        .collect()
}

fn want(expected: &[Expect]) -> Vec<Seen> {
    expected.iter().map(|(k, c, s, t)| (k.clone(), *c, *s, t.to_string())).collect()
}

/// The variant name, from `Debug`.
fn variant(k: &EventKind) -> String {
    format!("{k:?}").chars().take_while(char::is_ascii_alphanumeric).collect()
}

/// The yes/no field of the kinds that have one code per answer.
fn yes_no(k: &EventKind) -> Option<bool> {
    match k {
        Calibration { started } => Some(*started),
        AlarmFlag { set } | Flag { set, .. } => Some(*set),
        _ => None,
    }
}

/// Every kind (and every answer of a yes/no field) with its code, severity
/// and text, written by hand from upst 0.1.
fn all_kinds() -> Vec<Expect<'static>> {
    let d = Duration::from_millis(90_500);
    vec![
        (StartedOnBattery, "power.started_on_battery", Warn, "Running on battery"),
        (PowerLost, "power.lost", Warn, "Power failure, running on battery"),
        (PowerRestored { on_battery: d }, "power.restored", Good, "Power restored after 1m 31s on battery"),
        (
            LeftBattery { to: Power::Bypass, on_battery: d },
            "power.left_battery",
            Good,
            "Left battery power after 1m 31s on battery",
        ),
        (
            LeftBattery { to: Power::Unknown, on_battery: Duration::ZERO },
            "power.left_battery",
            Good,
            "Left battery power after 0s on battery",
        ),
        (Bypass, "power.bypass", Warn, "UPS switched to bypass, the load is unprotected"),
        (BypassEnded, "power.bypass.ended", Good, "Bypass ended"),
        (BatteryLow { charge: Some(19.5) }, "battery.low", Crit, "Battery low (19.5%)"),
        (BatteryLow { charge: None }, "battery.low", Crit, "Battery low"),
        (BatteryLowCleared, "battery.low.cleared", Good, "Battery no longer low"),
        (ReplaceBattery, "battery.replace", Warn, "UPS asks for a battery replacement"),
        (ReplaceBatteryCleared, "battery.replace.cleared", Good, "Battery replacement warning cleared"),
        (Calibration { started: true }, "battery.calibration.started", Info, "Calibration started"),
        (Calibration { started: false }, "battery.calibration.ended", Info, "Calibration ended"),
        (Overload, "load.overload", Crit, "UPS overloaded"),
        (OverloadCleared, "load.overload.cleared", Good, "Overload cleared"),
        (OutputOff, "output.off", Warn, "UPS output switched off"),
        (OutputOn, "output.on", Good, "UPS output back on"),
        (Trim, "input.trim", Info, "Input voltage high, trimming"),
        (Boost, "input.boost", Info, "Input voltage low, boosting"),
        (ForcedShutdown, "ups.forced_shutdown", Crit, "Forced shutdown in progress"),
        (Alarm { text: "Fan failure".into() }, "ups.alarm", Crit, "Alarm: Fan failure"),
        (AlarmFlag { set: true }, "ups.alarm.raised", Crit, "UPS alarm raised"),
        (AlarmFlag { set: false }, "ups.alarm.cleared", Good, "UPS alarm cleared"),
        (SelfTest { result: "Done and passed".into() }, "ups.self_test", Info, "Self-test: Done and passed"),
        (DriverLost, "driver.lost", Crit, "Driver lost communication with the UPS"),
        (DriverBack, "driver.back", Good, "Driver talks to the UPS again"),
        (DataStale { why: "DATA-STALE".into() }, "data.stale", Warn, "No data: DATA-STALE"),
        (DataFresh { was: "DATA-STALE".into() }, "data.fresh", Good, "UPS data is fresh again (was: DATA-STALE)"),
        (UpsdLost { why: "Connection refused".into() }, "upsd.lost", Crit, "Lost upsd: Connection refused"),
        (
            UpsdUnreachable { why: "Connection refused".into() },
            "upsd.unreachable",
            Crit,
            "Cannot reach upsd: Connection refused",
        ),
        (UpsdBack { server: "nas:3500".into() }, "upsd.back", Info, "Reconnected to nas:3500"),
        (Flag { flag: "HB".into(), set: true }, "status.flag.set", Info, "Status flag HB set"),
        (Flag { flag: "ECO".into(), set: false }, "status.flag.cleared", Info, "Status flag ECO cleared"),
    ]
}

/// What each status flag makes when it appears and when it goes away,
/// from upst 0.1's `flag_event`. `None`: no event.
fn flag_table() -> Vec<(&'static str, Option<Expect<'static>>, Option<Expect<'static>>)> {
    let flag = |f: &str, set| Flag { flag: f.into(), set };
    vec![
        ("OL", None, None),
        ("OB", Some((PowerLost, "power.lost", Warn, "Power failure, running on battery")), None),
        (
            "LB",
            Some((BatteryLow { charge: None }, "battery.low", Crit, "Battery low")),
            Some((BatteryLowCleared, "battery.low.cleared", Good, "Battery no longer low")),
        ),
        (
            "HB",
            Some((flag("HB", true), "status.flag.set", Info, "Status flag HB set")),
            Some((flag("HB", false), "status.flag.cleared", Info, "Status flag HB cleared")),
        ),
        (
            "RB",
            Some((ReplaceBattery, "battery.replace", Warn, "UPS asks for a battery replacement")),
            Some((ReplaceBatteryCleared, "battery.replace.cleared", Good, "Battery replacement warning cleared")),
        ),
        ("CHRG", None, None),
        ("DISCHRG", None, None),
        (
            "BYPASS",
            Some((Bypass, "power.bypass", Warn, "UPS switched to bypass, the load is unprotected")),
            Some((BypassEnded, "power.bypass.ended", Good, "Bypass ended")),
        ),
        (
            "CAL",
            Some((Calibration { started: true }, "battery.calibration.started", Info, "Calibration started")),
            Some((Calibration { started: false }, "battery.calibration.ended", Info, "Calibration ended")),
        ),
        (
            "OFF",
            Some((OutputOff, "output.off", Warn, "UPS output switched off")),
            Some((OutputOn, "output.on", Good, "UPS output back on")),
        ),
        (
            "OVER",
            Some((Overload, "load.overload", Crit, "UPS overloaded")),
            Some((OverloadCleared, "load.overload.cleared", Good, "Overload cleared")),
        ),
        ("TRIM", Some((Trim, "input.trim", Info, "Input voltage high, trimming")), None),
        ("BOOST", Some((Boost, "input.boost", Info, "Input voltage low, boosting")), None),
        ("FSD", Some((ForcedShutdown, "ups.forced_shutdown", Crit, "Forced shutdown in progress")), None),
        (
            "ALARM",
            Some((AlarmFlag { set: true }, "ups.alarm.raised", Crit, "UPS alarm raised")),
            Some((AlarmFlag { set: false }, "ups.alarm.cleared", Good, "UPS alarm cleared")),
        ),
        (
            "ECO",
            Some((flag("ECO", true), "status.flag.set", Info, "Status flag ECO set")),
            Some((flag("ECO", false), "status.flag.cleared", Info, "Status flag ECO cleared")),
        ),
        ("TEST", None, None),
        ("COMM", None, None),
        (
            "NOCOMM",
            Some((DriverLost, "driver.lost", Crit, "Driver lost communication with the UPS")),
            Some((DriverBack, "driver.back", Good, "Driver talks to the UPS again")),
        ),
        (
            "XYZ",
            Some((flag("XYZ", true), "status.flag.set", Info, "Status flag XYZ set")),
            Some((flag("XYZ", false), "status.flag.cleared", Info, "Status flag XYZ cleared")),
        ),
        // Flags are case-sensitive: this is not OB.
        (
            "ob",
            Some((flag("ob", true), "status.flag.set", Info, "Status flag ob set")),
            Some((flag("ob", false), "status.flag.cleared", Info, "Status flag ob cleared")),
        ),
    ]
}

#[test]
fn flag_transitions() {
    let is_power = |e: &Event| matches!(e.kind, PowerRestored { .. } | LeftBattery { .. });
    for (flag, set, cleared) in flag_table() {
        // On line, on battery, and with no status at all.
        for base in ["OL", "OB DISCHRG", ""] {
            if base.split_whitespace().any(|t| t == flag) {
                continue;
            }
            let with = if base.is_empty() { flag.to_string() } else { format!("{base} {flag}") };
            let ctx = format!("{base:?} <-> {with:?}");

            // The first poll makes no flag events.
            let mut u = ups();
            let first = poll(&mut u, status(&with), at(0));
            assert!(first.iter().all(|e| e.kind == StartedOnBattery), "{ctx}: {first:?}");

            let mut u = ups();
            poll(&mut u, status(base), at(0));
            let ev = poll(&mut u, status(&with), at(10));
            assert_eq!(seen(&ev), want(set.as_slice()), "{ctx}: set");
            assert!(poll(&mut u, status(&with), at(15)).is_empty(), "{ctx}: unchanged");

            // Only OB going away changes the power source, which is a power
            // event of its own (tested below), not a flag event.
            let ev = poll(&mut u, status(base), at(20));
            let (power, ev): (Vec<_>, Vec<_>) = ev.into_iter().partition(is_power);
            assert_eq!(power.len(), usize::from(flag == "OB"), "{ctx}: {power:?}");
            assert_eq!(seen(&ev), want(cleared.as_slice()), "{ctx}: cleared");
            assert!(poll(&mut u, status(base), at(25)).is_empty(), "{ctx}: unchanged");
        }
    }
}

#[test]
fn flag_events_come_in_order() {
    // Power first, then flags that appeared in the new order, then flags
    // that went away in the old order.
    let mut u = ups();
    poll(&mut u, status("OL OVER ALARM"), at(0));
    let ev = poll(&mut u, status("LB OB RB"), at(10));
    assert_eq!(
        seen(&ev),
        want(&[
            (BatteryLow { charge: None }, "battery.low", Crit, "Battery low"),
            (PowerLost, "power.lost", Warn, "Power failure, running on battery"),
            (ReplaceBattery, "battery.replace", Warn, "UPS asks for a battery replacement"),
            (OverloadCleared, "load.overload.cleared", Good, "Overload cleared"),
            (AlarmFlag { set: false }, "ups.alarm.cleared", Good, "UPS alarm cleared"),
        ])
    );
    let ev = poll(&mut u, status("OL CHRG CAL"), at(20));
    assert_eq!(
        seen(&ev),
        want(&[
            (
                PowerRestored { on_battery: Duration::from_secs(10) },
                "power.restored",
                Good,
                "Power restored after 10s on battery",
            ),
            (Calibration { started: true }, "battery.calibration.started", Info, "Calibration started"),
            (BatteryLowCleared, "battery.low.cleared", Good, "Battery no longer low"),
            (ReplaceBatteryCleared, "battery.replace.cleared", Good, "Battery replacement warning cleared"),
        ])
    );
}

#[test]
fn power_lost_and_restored() {
    // Outage lengths and their texts; whole seconds are rounded half away
    // from zero.
    let outages = [
        (Duration::from_millis(90_500), "1m 31s"),
        (Duration::from_millis(89_400), "1m 29s"),
        (Duration::new(59, 499_999_999), "59s"),
        (Duration::new(59, 500_000_000), "1m 00s"),
        (Duration::from_millis(400), "0s"),
        (Duration::from_secs(120), "2m 00s"),
        (Duration::from_millis(3_599_500), "1h 00m"),
        (Duration::from_secs(2 * 3600 + 5 * 60 + 29), "2h 05m"),
        (Duration::from_secs(5 * 86400 + 13 * 3600 + 1800), "5d 13h"),
    ];
    let mut u = ups();
    let mut now = t0();
    assert!(poll(&mut u, status("OL CHRG"), now).is_empty());
    assert_eq!((u.outages(), u.battery_secs(), u.outage_start()), (0, 0.0, None));
    let mut total = 0.0;
    for (i, (d, text)) in outages.into_iter().enumerate() {
        assert_eq!(upst_core::fmt::duration(d.as_secs_f64()), text);

        now += Duration::from_secs(60);
        let ev = poll(&mut u, status("OB DISCHRG"), now);
        assert_eq!(seen(&ev), want(&[(PowerLost, "power.lost", Warn, "Power failure, running on battery")]));
        assert_eq!(u.outages(), i as u32 + 1);
        assert_eq!(u.outage_start(), Some(now));
        assert_eq!(u.power_since(), Some((now, true)));
        assert!((u.battery_total(now + d) - (total + d.as_secs_f64())).abs() < 1e-6);
        assert!(poll(&mut u, status("OB DISCHRG"), now + d / 2).is_empty());

        now += d;
        let ev = poll(&mut u, status("OL CHRG"), now);
        let restored = format!("Power restored after {text} on battery");
        assert_eq!(seen(&ev), want(&[(PowerRestored { on_battery: d }, "power.restored", Good, &restored)]));
        total += d.as_secs_f64();
        assert!((u.battery_secs() - total).abs() < 1e-6, "{} != {total}", u.battery_secs());
        assert_eq!(u.battery_total(now + Duration::from_secs(99)), u.battery_secs());
        assert_eq!(u.outage_start(), None);
        assert_eq!(u.power_since(), Some((now, true)));
    }
    assert_eq!(u.outages(), outages.len() as u32);
}

#[test]
fn left_battery() {
    // Where OB goes, and the flag events that come after the power event.
    let cases: [(Option<&str>, Power, Vec<Expect>); 7] = [
        (
            Some("BYPASS"),
            Power::Bypass,
            vec![(Bypass, "power.bypass", Warn, "UPS switched to bypass, the load is unprotected")],
        ),
        (
            Some("OL BYPASS"),
            Power::Bypass,
            vec![(Bypass, "power.bypass", Warn, "UPS switched to bypass, the load is unprotected")],
        ),
        (Some("OFF"), Power::Off, vec![(OutputOff, "output.off", Warn, "UPS output switched off")]),
        (
            Some("OFF BYPASS"),
            Power::Bypass,
            vec![
                (OutputOff, "output.off", Warn, "UPS output switched off"),
                (Bypass, "power.bypass", Warn, "UPS switched to bypass, the load is unprotected"),
            ],
        ),
        (Some(""), Power::Unknown, vec![]),
        (Some("CHRG"), Power::Unknown, vec![]),
        // No ups.status at all.
        (None, Power::Unknown, vec![]),
    ];
    let d = Duration::from_millis(75_250);
    for (to, power, then) in cases {
        let mut u = ups();
        poll(&mut u, status("OL"), at(0));
        poll(&mut u, status("OB DISCHRG"), at(10));
        let list = to.map_or_else(|| vars(&[("battery.charge", "90")]), status);
        let now = at(10) + d;
        let ev = poll(&mut u, list, now);
        let mut expected = vec![(
            LeftBattery { to: power, on_battery: d },
            "power.left_battery",
            Good,
            "Left battery power after 1m 15s on battery",
        )];
        expected.extend(then);
        assert_eq!(seen(&ev), want(&expected), "OB -> {to:?}");
        assert_eq!(u.status().power(), power);
        assert_eq!((u.outages(), u.outage_start(), u.power_since()), (1, None, Some((now, true))));
        assert!((u.battery_secs() - 75.25).abs() < 1e-9);
    }

    // Back on line from bypass is no power restored: that was said already.
    let mut u = ups();
    poll(&mut u, status("OL"), at(0));
    poll(&mut u, status("OB"), at(10));
    poll(&mut u, status("BYPASS"), at(20));
    let ev = poll(&mut u, status("OL"), at(30));
    assert_eq!(seen(&ev), want(&[(BypassEnded, "power.bypass.ended", Good, "Bypass ended")]));

    // Bypass while on battery: OB wins, the power source stays.
    let mut u = ups();
    poll(&mut u, status("OB"), at(0));
    let ev = poll(&mut u, status("OB BYPASS"), at(10));
    assert_eq!(seen(&ev), want(&[(Bypass, "power.bypass", Warn, "UPS switched to bypass, the load is unprotected")]));
}

#[test]
fn started_on_battery() {
    // The first poll's status and the severity of "Running on battery":
    // that of the headline, at least Warn.
    let cases = [
        ("OB", Warn),
        ("OB DISCHRG", Warn),
        ("OB DISCHRG RB", Warn),
        ("OB BYPASS", Warn),
        ("OB ALARM", Warn),
        ("OB DISCHRG LB", Crit),
        ("LB OB", Crit),
        ("OB OVER", Crit),
        ("OB FSD", Crit),
    ];
    for (s, severity) in cases {
        let mut u = ups();
        let ev = poll(&mut u, status(s), t0());
        assert_eq!(
            seen(&ev),
            want(&[(StartedOnBattery, "power.started_on_battery", severity, "Running on battery")]),
            "{s}"
        );
        assert_eq!(u.outage_start(), Some(t0()), "{s}");
        assert_eq!(u.power_since(), Some((t0(), false)), "{s}");
        // Only outages seen to start count.
        assert_eq!(u.outages(), 0, "{s}");
        assert_eq!(u.battery_total(at(30)), 30.0, "{s}");
    }

    // Not on battery: nothing, whatever else is wrong.
    for s in ["OL", "OL LB", "OL OVER", "BYPASS", "OFF", "", "FSD", "XYZ"] {
        let mut u = ups();
        assert!(poll(&mut u, status(s), t0()).is_empty(), "{s}");
        assert_eq!(u.outage_start(), None, "{s}");
    }

    // The outage started at the first poll.
    let mut u = ups();
    poll(&mut u, status("OB DISCHRG"), t0());
    let ev = poll(&mut u, status("OL CHRG"), at(42));
    assert_eq!(
        seen(&ev),
        want(&[(
            PowerRestored { on_battery: Duration::from_secs(42) },
            "power.restored",
            Good,
            "Power restored after 42s on battery"
        )])
    );
    assert_eq!((u.outages(), u.battery_secs(), u.outage_start()), (0, 42.0, None));
}

#[test]
fn battery_low_charge() {
    // battery.charge at the poll that brings LB, what the event holds,
    // and its text.
    let cases = [
        (Some("19"), Some(19.0), "Battery low (19%)"),
        (Some("19.5"), Some(19.5), "Battery low (19.5%)"),
        (Some("19.0"), Some(19.0), "Battery low (19%)"),
        (Some(" 7 "), Some(7.0), "Battery low (7%)"),
        (Some("0"), Some(0.0), "Battery low (0%)"),
        (Some("100"), Some(100.0), "Battery low (100%)"),
        (None, None, "Battery low"),
        (Some(""), None, "Battery low"),
        (Some("n/a"), None, "Battery low"),
        (Some("NaN"), None, "Battery low"),
        (Some("inf"), None, "Battery low"),
    ];
    for base in ["OB DISCHRG", "OL"] {
        for (charge, held, text) in cases {
            let mut u = ups();
            poll(&mut u, vars(&[("ups.status", base), ("battery.charge", "50")]), at(0));
            let mut list = status(&format!("{base} LB"));
            list.extend(charge.map(|c| ("battery.charge".to_string(), c.to_string())));
            let ev = poll(&mut u, list, at(10));
            assert_eq!(
                seen(&ev),
                want(&[(BatteryLow { charge: held }, "battery.low", Crit, text)]),
                "{base} {charge:?}"
            );
        }
    }
}

#[test]
fn alarm_variable() {
    let alarm = |v: Option<&str>| {
        let mut list = vars(&[("ups.status", "OL"), ("battery.charge", "100")]);
        list.extend(v.map(|v| ("ups.alarm".to_string(), v.to_string())));
        list
    };
    let raised = |text: &str| vec![(Alarm { text: text.into() }, "ups.alarm", Crit, format!("Alarm: {text}"))];
    let mut u = ups();
    // Present on the first poll: no event.
    assert!(poll(&mut u, alarm(Some("Fan failure")), at(0)).is_empty());
    let steps: [(Option<&str>, Vec<_>); 11] = [
        (Some("Fan failure"), vec![]),
        (Some("Replace battery!"), raised("Replace battery!")),
        (Some("Replace battery!"), vec![]),
        (Some("  Overheat  "), raised("Overheat")),
        // The value changed, if not its trimmed text.
        (Some("Overheat"), raised("Overheat")),
        (Some(""), vec![]),
        (Some("   "), vec![]),
        (None, vec![]),
        // Gone and back counts as a change.
        (Some("Overheat"), raised("Overheat")),
        (None, vec![]),
        (Some("Overheat"), raised("Overheat")),
    ];
    for (i, (value, expected)) in steps.into_iter().enumerate() {
        let ev = poll(&mut u, alarm(value), at(10 * (i as u64 + 1)));
        let expected: Vec<_> = expected.iter().map(|(k, c, s, t)| (k.clone(), *c, *s, t.as_str())).collect();
        assert_eq!(seen(&ev), want(&expected), "step {i}: {value:?}");
    }

    // A second poll at the same time with the same value is no change.
    let mut u = ups();
    poll(&mut u, alarm(None), at(0));
    assert_eq!(poll(&mut u, alarm(Some("Overheat")), at(10)).len(), 1);
    assert!(poll(&mut u, alarm(Some("Overheat")), at(10)).is_empty());
    let var = u.var("ups.alarm").unwrap();
    assert_eq!((var.prev.as_deref(), var.changed), (None, Some(at(10))));
}

#[test]
fn self_test_variable() {
    let test = |v: Option<&str>| {
        let mut list = status("OL");
        list.extend(v.map(|v| ("ups.test.result".to_string(), v.to_string())));
        list
    };
    let result = |r: &str| vec![(SelfTest { result: r.into() }, "ups.self_test", Info, format!("Self-test: {r}"))];
    let mut u = ups();
    assert!(poll(&mut u, test(Some("No test initiated")), at(0)).is_empty());
    let steps: [(Option<&str>, Vec<_>); 7] = [
        (Some("No test initiated"), vec![]),
        (Some("In progress"), result("In progress")),
        (Some(" Done and passed "), result("Done and passed")),
        (Some(" Done and passed "), vec![]),
        (Some(""), vec![]),
        (None, vec![]),
        (Some("Done and warning"), result("Done and warning")),
    ];
    for (i, (value, expected)) in steps.into_iter().enumerate() {
        let ev = poll(&mut u, test(value), at(10 * (i as u64 + 1)));
        let expected: Vec<_> = expected.iter().map(|(k, c, s, t)| (k.clone(), *c, *s, t.as_str())).collect();
        assert_eq!(seen(&ev), want(&expected), "step {i}: {value:?}");
    }

    // Status events first, then the alarm, then the self-test.
    let mut u = ups();
    poll(&mut u, status("OL"), at(0));
    let list = vars(&[("ups.status", "OL RB ALARM"), ("ups.alarm", "Battery bad"), ("ups.test.result", "Failed")]);
    let ev = poll(&mut u, list, at(10));
    assert_eq!(
        seen(&ev),
        want(&[
            (ReplaceBattery, "battery.replace", Warn, "UPS asks for a battery replacement"),
            (AlarmFlag { set: true }, "ups.alarm.raised", Crit, "UPS alarm raised"),
            (Alarm { text: "Battery bad".into() }, "ups.alarm", Crit, "Alarm: Battery bad"),
            (SelfTest { result: "Failed".into() }, "ups.self_test", Info, "Self-test: Failed"),
        ])
    );
}

#[test]
fn link_events() {
    let mut u = ups();
    assert_eq!((u.link(), u.is_live()), (&Link::Connecting, false));

    // upsd is unreachable before any poll.
    let e = lose(&mut u, down("Connection refused"), at(1));
    let unreachable = UpsdUnreachable { why: "Connection refused".into() };
    assert_eq!(
        seen(e.as_slice()),
        want(&[(unreachable, "upsd.unreachable", Crit, "Cannot reach upsd: Connection refused")])
    );
    assert_eq!((u.link(), u.is_live()), (&down("Connection refused"), false));
    // Telling the same, or another reason, is no event.
    assert_eq!(lose(&mut u, down("Connection refused"), at(2)), None);
    assert_eq!(lose(&mut u, down("timed out"), at(3)), None);
    assert_eq!(u.link(), &down("timed out"));

    // The first poll after that reconnects (as upst 0.1's replay said).
    let ev = poll(&mut u, status("OL"), at(4));
    let back = || (UpsdBack { server: "localhost".into() }, "upsd.back", Info, "Reconnected to localhost");
    assert_eq!(seen(&ev), want(&[back()]));
    assert_eq!((u.link(), u.is_live()), (&Link::Up, true));
    assert!(poll(&mut u, status("OL"), at(5)).is_empty());

    // Up -> Stale -> Up.
    let e = lose(&mut u, stale("DATA-STALE"), at(6));
    let no_data = || (DataStale { why: "DATA-STALE".into() }, "data.stale", Warn, "No data: DATA-STALE");
    assert_eq!(seen(e.as_slice()), want(&[no_data()]));
    assert_eq!((u.link(), u.is_live()), (&stale("DATA-STALE"), false));
    assert_eq!(lose(&mut u, stale("DATA-STALE"), at(7)), None);
    let ev = poll(&mut u, status("OL"), at(8));
    let fresh = |was: &str| {
        (DataFresh { was: was.into() }, "data.fresh", Good, format!("UPS data is fresh again (was: {was})"))
    };
    let (k, c, s, t) = fresh("DATA-STALE");
    assert_eq!(seen(&ev), want(&[(k, c, s, &t)]));
    assert!(u.is_live());
    assert!(poll(&mut u, status("OL"), at(9)).is_empty());

    // Up -> Down.
    let e = lose(&mut u, down("Connection reset"), at(10));
    let lost = |why: &str| (UpsdLost { why: why.into() }, "upsd.lost", Crit, format!("Lost upsd: {why}"));
    let (k, c, s, t) = lost("Connection reset");
    assert_eq!(seen(e.as_slice()), want(&[(k, c, s, &t)]));
    assert_eq!((u.link(), u.is_live()), (&down("Connection reset"), false));
    assert_eq!(lose(&mut u, down("Connection reset"), at(11)), None);
    // Down -> Stale: upsd answers again, but that is no event.
    assert_eq!(lose(&mut u, stale("DRIVER-NOT-CONNECTED"), at(12)), None);
    assert_eq!(u.link(), &stale("DRIVER-NOT-CONNECTED"));
    // Stale -> Down.
    let e = lose(&mut u, down("Connection refused"), at(13));
    let (k, c, s, t) = lost("Connection refused");
    assert_eq!(seen(e.as_slice()), want(&[(k, c, s, &t)]));
    assert_eq!(lose(&mut u, stale("DRIVER-NOT-CONNECTED"), at(14)), None);
    // Data again: reconnected, then fresh.
    let ev = poll(&mut u, status("OL"), at(15));
    let (k, c, s, t) = fresh("DRIVER-NOT-CONNECTED");
    assert_eq!(seen(&ev), want(&[back(), (k, c, s, &t)]));
    assert!(poll(&mut u, status("OL"), at(16)).is_empty());

    // Stale before any poll.
    let mut u = ups();
    let e = lose(&mut u, stale("DATA-STALE"), at(1));
    assert_eq!(seen(e.as_slice()), want(&[no_data()]));
    let ev = poll(&mut u, status("OB"), at(2));
    let (k, c, s, t) = fresh("DATA-STALE");
    assert_eq!(
        seen(&ev),
        want(&[(k, c, s, &t), (StartedOnBattery, "power.started_on_battery", Warn, "Running on battery")])
    );
}

#[test]
fn upsd_back() {
    // Exactly once after any number of losses, and before anything else.
    let mut u = Ups::builder("rack", "nas:3500").history(None).build();
    poll(&mut u, vars(&[("ups.status", "OL"), ("battery.charge", "100")]), at(0));
    assert!(lose(&mut u, down("Connection refused"), at(10)).is_some());
    assert!(lose(&mut u, down("Connection refused"), at(11)).is_none());
    assert!(lose(&mut u, down("No route to host"), at(12)).is_none());
    let list = vars(&[("ups.status", "OB DISCHRG"), ("battery.charge", "99"), ("ups.alarm", "On battery")]);
    let ev = poll(&mut u, list.clone(), at(20));
    assert_eq!(
        seen(&ev),
        want(&[
            (UpsdBack { server: "nas:3500".into() }, "upsd.back", Info, "Reconnected to nas:3500"),
            (PowerLost, "power.lost", Warn, "Power failure, running on battery"),
            (Alarm { text: "On battery".into() }, "ups.alarm", Crit, "Alarm: On battery"),
        ])
    );
    assert!(poll(&mut u, list, at(30)).is_empty());

    // Stale data is no loss of upsd.
    assert!(lose(&mut u, stale("DATA-STALE"), at(40)).is_some());
    let ev = poll(&mut u, status("OB DISCHRG"), at(50));
    assert_eq!(ev.iter().map(Event::code).collect::<Vec<_>>(), ["data.fresh"]);

    // Neither is a gap in the record.
    u.gap(at(60));
    assert_eq!((u.link(), u.is_live()), (&down("not recorded"), false));
    assert!(poll(&mut u, status("OB DISCHRG"), at(70)).is_empty());
    assert!(u.is_live());
}

#[test]
fn every_kind_has_its_code_severity_and_text() {
    for (kind, code, severity, text) in all_kinds() {
        let e = Event::new(at(7), kind.clone());
        assert_eq!(
            (e.at, &e.kind, e.code(), e.severity, e.to_string()),
            (at(7), &kind, code, severity, text.to_string())
        );
        assert_eq!(kind.code(), code);
        assert_eq!(kind.to_string(), text);
    }
    // The power source a battery is left for does not change the code.
    for to in [Power::Online, Power::Battery, Power::Bypass, Power::Off, Power::Unknown] {
        assert_eq!(LeftBattery { to, on_battery: Duration::ZERO }.code(), "power.left_battery");
    }
}

/// The rows of the markdown table that starts with `header` in a doc
/// comment of `src`, split into trimmed cells.
fn doc_table(src: &str, header: &str) -> Vec<Vec<String>> {
    let lines: Vec<&str> = src.lines().map(str::trim).collect();
    let start = lines.iter().position(|l| *l == header).unwrap_or_else(|| panic!("no table {header:?}"));
    assert_eq!(lines[start + 1], "/// |---|---|");
    lines[start + 2..]
        .iter()
        .take_while(|l| l.starts_with("/// |"))
        .map(|l| l["/// |".len()..].trim_end_matches('|').split('|').map(|c| c.trim().to_string()).collect())
        .collect()
}

fn unquote(cell: &str) -> &str {
    cell.strip_prefix('`').and_then(|c| c.strip_suffix('`')).unwrap_or_else(|| panic!("not code: {cell:?}"))
}

#[test]
fn event_codes_match_doc_table() {
    let src = include_str!("../src/event.rs");
    let rows = doc_table(src, "/// | Code | Kind |");
    let kinds = all_kinds();
    let mut documented = BTreeSet::new();
    for row in &rows {
        let code = unquote(&row[0]);
        let cell = &row[1];
        // "[`Name`](EventKind::Name)", maybe followed by "`{ field: true }`".
        let name = &cell[cell.find("[`").unwrap() + 2..cell.find("`]").unwrap()];
        assert!(cell.contains(&format!("(EventKind::{name})")), "{cell}");
        let field = cell.split_once("`{ ").map(|(_, f)| f.trim_end_matches(" }`"));
        let answer = field.map(|f| f.ends_with("true"));
        let matching: Vec<_> = kinds.iter().filter(|(k, ..)| variant(k) == name && yes_no(k) == answer).collect();
        assert!(!matching.is_empty(), "no kind for row {row:?}");
        for (k, ..) in matching {
            assert_eq!(k.code(), code, "{k:?}");
            if let Some(f) = field {
                assert!(format!("{k:?}").contains(f), "{k:?} has no {f}");
            }
        }
        assert!(documented.insert((name.to_string(), answer)), "row twice: {row:?}");
    }
    // Every kind (and answer) is documented.
    let tested: BTreeSet<_> = kinds.iter().map(|(k, ..)| (variant(k), yes_no(k))).collect();
    assert_eq!(documented, tested);

    // Every variant of the enum is in all_kinds, so in the table too.
    let body = &src[src.find("pub enum EventKind {").unwrap()..];
    let body = &body[..body.find("\n}").unwrap()];
    let variants: BTreeSet<String> = body
        .lines()
        .filter(|l| l.len() > 4 && l.starts_with("    ") && l.as_bytes()[4].is_ascii_uppercase())
        .map(|l| l.trim().chars().take_while(char::is_ascii_alphanumeric).collect())
        .collect();
    let named: BTreeSet<String> = kinds.iter().map(|(k, ..)| variant(k)).collect();
    assert_eq!(variants, named);
}

#[test]
fn event_codes_unique_and_well_formed() {
    let mut by_key = BTreeMap::new();
    for (kind, ..) in all_kinds() {
        let code = kind.code();
        let ok = code.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        assert!(ok, "{code} is not [a-z_]+(.[a-z_]+)*");
        let key = (variant(&kind), yes_no(&kind));
        assert_eq!(*by_key.entry(key).or_insert(code), code, "{kind:?}");
    }
    let codes: BTreeSet<_> = by_key.values().collect();
    assert_eq!(codes.len(), by_key.len(), "two kinds share a code: {by_key:?}");
}

#[test]
fn flag_codes_match_doc_table() {
    let known = [
        "OL", "OB", "LB", "HB", "RB", "CHRG", "DISCHRG", "BYPASS", "CAL", "OFF", "OVER", "TRIM", "BOOST", "FSD",
        "ALARM", "ECO", "TEST", "COMM", "NOCOMM",
    ];
    let rows = doc_table(include_str!("../src/status.rs"), "/// | Flag | Code |");
    let mut documented = Vec::new();
    let mut codes = BTreeSet::new();
    for row in &rows {
        let code = unquote(&row[1]);
        let snake = code.split('_').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase()));
        assert!(snake, "{code} is not snake_case");
        assert!(codes.insert(code), "{code} twice");
        if row[0] == "anything else" {
            for flag in ["XYZ", "", "ol", "OB2", "LB "] {
                let info = flag_info(flag);
                assert_eq!((info.code, info.label, info.severity), (code, "", Info), "{flag:?}");
            }
        } else {
            let flag = unquote(&row[0]);
            let info = flag_info(flag);
            assert_eq!(info.code, code, "{flag}");
            assert!(!info.label.is_empty() && !info.meaning.is_empty(), "{flag}");
            documented.push(flag);
        }
    }
    assert_eq!(documented, known);
    assert!(codes.contains("unknown"));
}

#[test]
fn events_carry_their_time() {
    // Every path that makes events, each at its own time.
    let mut u = Ups::new("ups", "localhost");
    let mut events = Vec::new();
    let mut apply = |u: &mut Ups, list, t| events.extend(u.apply(list, at(t)).into_iter().map(|e| (t, e)));
    apply(&mut u, status("OB LB"), 1);
    apply(&mut u, status("OL RB"), 2);
    apply(&mut u, vars(&[("ups.status", "OB OVER CAL"), ("ups.alarm", "x"), ("ups.test.result", "y")]), 3);
    apply(&mut u, status("BYPASS NOCOMM ECO"), 4);
    apply(&mut u, status("OL"), 5);
    let mut lost = Vec::new();
    for (t, link) in [(6, stale("a")), (7, down("b")), (9, down("c")), (10, stale("d"))] {
        lost.extend(u.lost(link, at(t)).map(|e| (t, e)));
    }
    apply(&mut u, status("OL"), 11);
    events.extend(lost);
    assert_eq!(events.len(), 23, "{events:?}");
    for (t, e) in &events {
        assert_eq!(e.at, at(*t), "{e:?}");
    }
    let mut u = ups();
    assert_eq!(u.lost(down("x"), at(12)).map(|e| e.at), Some(at(12)));
}

#[test]
fn types_are_send_and_sync() {
    fn both<T: Send + Sync + 'static>() {}
    both::<Event>();
    both::<EventKind>();
    both::<Ups>();
}

#[cfg(feature = "serde")]
mod json {
    use super::*;
    use serde_json::{Value, json};
    use upst_core::history::Trend;
    use upst_core::{Estimate, Status, Var};

    fn round_trip<T>(x: &T) -> Value
    where
        T: ::serde::Serialize + ::serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let text = serde_json::to_string(x).unwrap();
        let back: T = serde_json::from_str(&text).unwrap();
        assert_eq!(&back, x, "{text}");
        serde_json::from_str(&text).unwrap()
    }

    fn snake(name: &str) -> String {
        let mut s = String::new();
        for (i, c) in name.chars().enumerate() {
            if c.is_ascii_uppercase() && i > 0 {
                s.push('_');
            }
            s.push(c.to_ascii_lowercase());
        }
        s
    }

    #[test]
    fn events_round_trip() {
        let mut events: Vec<_> = all_kinds().into_iter().map(|(k, ..)| Event::new(at(5), k)).collect();
        // A severity other than the kind's own.
        events.push(Event { at: t0() + Duration::new(1, 1), severity: Crit, kind: StartedOnBattery });
        for e in &events {
            let v = round_trip(e);
            assert_eq!(v["kind"]["type"], json!(snake(&variant(&e.kind))), "{v}");
            assert_eq!(v["severity"], json!(format!("{:?}", e.severity).to_lowercase()), "{v}");
        }
    }

    #[test]
    fn event_json_shape() {
        let e = Event::new(t0(), PowerRestored { on_battery: Duration::from_millis(90_500) });
        assert_eq!(
            round_trip(&e),
            json!({
                "at": {"secs_since_epoch": 1_700_000_000u64, "nanos_since_epoch": 0},
                "severity": "good",
                "kind": {"type": "power_restored", "on_battery": {"secs": 90, "nanos": 500_000_000}},
            })
        );
        let kinds = [
            (PowerLost, json!({"type": "power_lost"})),
            (
                LeftBattery { to: Power::Bypass, on_battery: Duration::from_secs(3) },
                json!({"type": "left_battery", "to": "bypass", "on_battery": {"secs": 3, "nanos": 0}}),
            ),
            (BatteryLow { charge: Some(19.5) }, json!({"type": "battery_low", "charge": 19.5})),
            (BatteryLow { charge: None }, json!({"type": "battery_low", "charge": null})),
            (Calibration { started: false }, json!({"type": "calibration", "started": false})),
            (Alarm { text: "x".into() }, json!({"type": "alarm", "text": "x"})),
            (UpsdBack { server: "nas:3500".into() }, json!({"type": "upsd_back", "server": "nas:3500"})),
            (Flag { flag: "HB".into(), set: true }, json!({"type": "flag", "flag": "HB", "set": true})),
        ];
        for (k, v) in kinds {
            assert_eq!(round_trip(&k), v);
        }
        assert!(serde_json::from_value::<EventKind>(json!({"type": "nope"})).is_err());
    }

    #[test]
    fn enums_and_status_json() {
        for (s, v) in [(Good, "good"), (Info, "info"), (Warn, "warn"), (Crit, "crit")] {
            assert_eq!(round_trip(&s), json!(v));
        }
        let powers = [
            (Power::Online, "online"),
            (Power::Battery, "battery"),
            (Power::Bypass, "bypass"),
            (Power::Off, "off"),
            (Power::Unknown, "unknown"),
        ];
        for (p, v) in powers {
            assert_eq!(round_trip(&p), json!(v));
        }
        assert_eq!(round_trip(&Status::parse("OB DISCHRG  LB")), json!(["OB", "DISCHRG", "LB"]));
        assert_eq!(round_trip(&Status::parse("")), json!([]));
        let links = [
            (Link::Connecting, json!({"state": "connecting"})),
            (Link::Up, json!({"state": "up"})),
            (stale("DATA-STALE"), json!({"state": "stale", "why": "DATA-STALE"})),
            (down("refused"), json!({"state": "down", "why": "refused"})),
        ];
        for (l, v) in links {
            assert_eq!(round_trip(&l), v);
        }
    }

    #[test]
    fn estimate_and_var_round_trip() {
        // Estimate is non_exhaustive, so it comes from JSON here.
        let mut e: Estimate = serde_json::from_value(json!({
            "trend": "discharging",
            "per_hour": -60.0,
            "rough": true,
            "to_low": 1560.0,
            "to_empty": 2760.5,
            "to_full": null,
            "watts": 12.25,
        }))
        .unwrap();
        assert_eq!((e.trend, e.to_low, e.watts), (Trend::Discharging, Some(1560.0), Some(12.25)));
        let v = round_trip(&e);
        assert_eq!((&v["trend"], &v["to_full"], &v["per_hour"]), (&json!("discharging"), &Value::Null, &json!(-60.0)));
        (e.trend, e.to_low, e.to_empty, e.to_full, e.watts) = (Trend::Charging, None, None, Some(30.0), None);
        assert_eq!(round_trip(&e)["trend"], json!("charging"));

        let var = Var { value: "230".into(), prev: Some("229".into()), changed: Some(at(3)) };
        assert_eq!(
            round_trip(&var),
            json!({
                "value": "230",
                "prev": "229",
                "changed": {"secs_since_epoch": 1_700_000_003u64, "nanos_since_epoch": 0},
            })
        );
        round_trip(&Var { value: String::new(), prev: None, changed: None });
        // And one from a real UPS.
        let mut u = ups();
        u.apply(vars(&[("input.voltage", "229")]), at(0));
        u.apply(vars(&[("input.voltage", "230")]), at(1));
        assert_eq!(round_trip(u.var("input.voltage").unwrap())["prev"], json!("229"));
    }
}
