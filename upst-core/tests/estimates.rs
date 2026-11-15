//! Estimates: synthetic charge curves with exact numbers, and recorded
//! discharges of the demo UPSes.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use upst_core::history::{History, Trend};
use upst_core::{Link, Ups};

const T0: u64 = 1_700_000_000;

/// The time `secs` after (or before) T0.
fn at(secs: f64) -> SystemTime {
    let t0 = UNIX_EPOCH + Duration::from_secs(T0);
    if secs < 0.0 { t0 - Duration::from_secs_f64(-secs) } else { t0 + Duration::from_secs_f64(secs) }
}

fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn ups() -> Ups {
    Ups::builder("ups", "localhost").history(Some(History::new(86400.0, 2.0))).build()
}

/// A poll with `battery.charge.low` at 20 %.
fn poll(u: &mut Ups, secs: f64, status: &str, charge: f64) {
    let charge = charge.to_string();
    u.apply(vars(&[("ups.status", status), ("battery.charge", &charge), ("battery.charge.low", "20")]), at(secs));
}

/// 50 % falling 1 % a minute for four minutes: 46 % at 240 s.
fn discharge(u: &mut Ups) {
    for i in 0..=4 {
        poll(u, i as f64 * 60.0, "OB DISCHRG", 50.0 - i as f64);
    }
}

#[track_caller]
fn close(got: f64, want: f64) {
    assert!((got - want).abs() <= 1e-9 * want.abs().max(1.0), "{got} != {want}");
}

#[track_caller]
fn close_opt(got: Option<f64>, want: f64) {
    close(got.unwrap_or_else(|| panic!("None, want {want}")), want);
}

#[test]
fn steady_discharge() {
    let mut u = ups();
    discharge(&mut u);
    let e = u.estimate(at(240.0)).unwrap();
    assert_eq!((e.trend, e.rough), (Trend::Discharging, false));
    close(e.per_hour, -60.0);
    // 46 % now, low at 20 %.
    close_opt(e.to_low, 26.0 * 60.0);
    close_opt(e.to_empty, 46.0 * 60.0);
    assert_eq!((e.to_full, e.watts), (None, None));
    // Between steps the rate holds.
    assert_eq!(u.estimate(at(270.0)), Some(e));
}

#[test]
fn single_step_is_rough() {
    let mut u = ups();
    poll(&mut u, 0.0, "OB DISCHRG", 50.0);
    poll(&mut u, 30.0, "OB DISCHRG", 50.0);
    assert_eq!(u.estimate(at(30.0)), None);
    poll(&mut u, 60.0, "OB DISCHRG", 49.0);
    let e = u.estimate(at(60.0)).unwrap();
    assert!(e.rough);
    close(e.per_hour, -60.0);
    close_opt(e.to_low, 29.0 * 60.0);
    // One step, measured from the start of the segment up to now.
    let e = u.estimate(at(120.0)).unwrap();
    assert!(e.rough);
    close(e.per_hour, -30.0);
    close_opt(e.to_empty, 49.0 * 120.0);
    poll(&mut u, 120.0, "OB DISCHRG", 48.0);
    assert!(!u.estimate(at(120.0)).unwrap().rough);
}

#[test]
fn long_pause_falls_back_to_the_average() {
    let mut u = ups();
    discharge(&mut u);
    // A step every 60 s; up to 1.5 times that without one, the rate holds.
    close(u.estimate(at(330.0)).unwrap().per_hour, -60.0);
    // Then it is the average from the oldest step used (49 % at 60 s) to now.
    let e = u.estimate(at(331.0)).unwrap();
    assert!(!e.rough);
    close(e.per_hour, -3.0 / 271.0 * 3600.0);
    close_opt(e.to_empty, 46.0 * 271.0 / 3.0);
    close(u.estimate(at(600.0)).unwrap().per_hour, -20.0);
}

#[test]
fn rate_follows_the_last_twelve_steps() {
    let mut u = ups();
    poll(&mut u, 0.0, "OB DISCHRG", 90.0);
    // Ten steps 30 s apart, then twelve 60 s apart.
    let mut t = 0.0;
    for i in 1..=22 {
        t += if i <= 10 { 30.0 } else { 60.0 };
        poll(&mut u, t, "OB DISCHRG", 90.0 - i as f64);
    }
    close(u.estimate(at(t)).unwrap().per_hour, -60.0);
}

#[test]
fn charging() {
    let mut u = ups();
    for i in 0..=4 {
        poll(&mut u, i as f64 * 60.0, "OL CHRG", 50.0 + i as f64);
    }
    let e = u.estimate(at(240.0)).unwrap();
    assert_eq!((e.trend, e.rough), (Trend::Charging, false));
    close(e.per_hour, 60.0);
    close_opt(e.to_full, 46.0 * 60.0);
    assert_eq!((e.to_low, e.to_empty, e.watts), (None, None, None));
}

#[test]
fn online_below_full_counts_as_charging() {
    // The EcoFlow never says CHRG.
    let mut u = ups();
    for i in 0..=4 {
        poll(&mut u, i as f64 * 60.0, "OL", 95.0 + i as f64);
    }
    let e = u.estimate(at(240.0)).unwrap();
    assert_eq!(e.trend, Trend::Charging);
    close(e.per_hour, 60.0);
    close_opt(e.to_full, 60.0);
    // Full on line is no trend at all.
    poll(&mut u, 300.0, "OL", 100.0);
    assert_eq!(u.estimate(at(300.0)), None);
    assert_eq!(u.tracker().trend(), None);
    // On battery without DISCHRG still discharges.
    poll(&mut u, 360.0, "OB", 100.0);
    poll(&mut u, 420.0, "OB", 99.0);
    assert_eq!(u.estimate(at(420.0)).unwrap().trend, Trend::Discharging);
}

#[test]
fn time_to_low_needs_a_threshold_above_the_charge() {
    // No battery.charge.low.
    let mut u = ups();
    for i in 0..=2 {
        let charge = (50 - i).to_string();
        u.apply(vars(&[("ups.status", "OB DISCHRG"), ("battery.charge", &charge)]), at(i as f64 * 60.0));
    }
    let e = u.estimate(at(120.0)).unwrap();
    assert_eq!(e.to_low, None);
    close_opt(e.to_empty, 48.0 * 60.0);

    // A threshold of 0 is none either.
    let mut u = ups();
    for i in 0..=2 {
        let charge = (50 - i).to_string();
        let list = vars(&[("ups.status", "OB DISCHRG"), ("battery.charge", &charge), ("battery.charge.low", "0")]);
        u.apply(list, at(i as f64 * 60.0));
    }
    assert_eq!(u.estimate(at(120.0)).unwrap().to_low, None);

    // At or below the threshold there is nothing left to wait for.
    let mut u = ups();
    for (i, charge) in [23.0, 22.0, 21.0, 20.0, 19.0].into_iter().enumerate() {
        poll(&mut u, i as f64 * 60.0, "OB DISCHRG LB", charge);
        let e = u.estimate(at(i as f64 * 60.0));
        match charge {
            23.0 => assert_eq!(e, None),
            22.0 => close_opt(e.unwrap().to_low, 120.0),
            21.0 => close_opt(e.unwrap().to_low, 60.0),
            _ => assert_eq!(e.unwrap().to_low, None),
        }
    }
    close_opt(u.estimate(at(240.0)).unwrap().to_empty, 19.0 * 60.0);
}

type Lose = fn(&mut Ups, SystemTime);

#[test]
fn link_loss_and_gaps_reset_the_tracker() {
    let losses: [(&str, Lose); 3] = [
        ("down", |u, t| {
            u.lost(Link::Down("gone".into()), t);
        }),
        ("stale", |u, t| {
            u.lost(Link::Stale("Data stale".into()), t);
        }),
        ("gap", |u, t| u.gap(t)),
    ];
    for (name, lose) in losses {
        let mut u = ups();
        discharge(&mut u);
        assert!(u.estimate(at(250.0)).is_some());
        lose(&mut u, at(250.0));
        assert_eq!(u.estimate(at(250.0)), None, "{name}");
        assert_eq!(u.tracker().trend(), None, "{name}");
        // The next poll starts a new segment, so one step makes a rough rate.
        poll(&mut u, 300.0, "OB DISCHRG", 45.0);
        assert_eq!(u.estimate(at(300.0)), None, "{name}");
        poll(&mut u, 330.0, "OB DISCHRG", 44.0);
        let e = u.estimate(at(330.0)).unwrap();
        assert!(e.rough, "{name}");
        close(e.per_hour, -120.0);
    }
}

#[test]
fn a_change_of_direction_starts_over() {
    let mut u = ups();
    discharge(&mut u);
    poll(&mut u, 300.0, "OL CHRG", 46.0);
    assert_eq!(u.estimate(at(300.0)), None);
    poll(&mut u, 360.0, "OL CHRG", 47.0);
    let e = u.estimate(at(360.0)).unwrap();
    assert_eq!((e.trend, e.rough), (Trend::Charging, true));
    close(e.per_hour, 60.0);
    close_opt(e.to_full, 53.0 * 60.0);
    // And back.
    poll(&mut u, 420.0, "OB DISCHRG", 47.0);
    assert_eq!(u.estimate(at(420.0)), None);
}

#[test]
fn capacity_gives_watts() {
    let mut u = ups();
    discharge(&mut u);
    assert_eq!(u.estimate(at(240.0)).unwrap().watts, None);
    // 60 %/h of 1000 Wh.
    u.set_capacity_wh(Some(1000.0));
    close_opt(u.estimate(at(240.0)).unwrap().watts, 600.0);
    u.set_capacity_wh(None);
    assert_eq!(u.estimate(at(240.0)).unwrap().watts, None);

    let mut u = Ups::builder("ups", "localhost").capacity_wh(Some(2000.0)).build();
    discharge(&mut u);
    close_opt(u.estimate(at(240.0)).unwrap().watts, 1200.0);
}

fn ecoflow(u: &mut Ups, secs: f64, status: &str, charge: f64) {
    let charge = charge.to_string();
    let list = vars(&[
        ("device.mfr", "EcoFlow"),
        ("ups.power.nominal", "2048"),
        ("ups.status", status),
        ("battery.charge", &charge),
    ]);
    u.apply(list, at(secs));
}

#[test]
fn explicit_capacity_beats_the_ecoflow_guess() {
    let run = |u: &mut Ups| {
        for i in 0..=4 {
            ecoflow(u, i as f64 * 60.0, "OB", 50.0 - i as f64);
        }
        u.estimate(at(240.0)).unwrap().watts
    };
    let mut u = Ups::new("eco", "localhost");
    close_opt(run(&mut u), 0.6 * 2048.0);
    assert!(u.capacity_guessed());
    u.set_capacity_wh(Some(1000.0));
    assert!(!u.capacity_guessed());
    close_opt(u.estimate(at(240.0)).unwrap().watts, 600.0);
    u.set_capacity_wh(None);
    assert!(u.capacity_guessed());
    close_opt(u.estimate(at(240.0)).unwrap().watts, 0.6 * 2048.0);

    let mut u = Ups::builder("eco", "localhost").capacity_wh(Some(500.0)).build();
    close_opt(run(&mut u), 300.0);
    assert_eq!((u.capacity_wh(), u.capacity_guessed()), (Some(500.0), false));
}

#[test]
fn same_without_history() {
    let script = |u: &mut Ups| {
        discharge(u);
        u.lost(Link::Down("gone".into()), at(250.0));
        for i in 0..6 {
            poll(u, 300.0 + i as f64 * 40.0, "OL CHRG", 45.0 + i as f64);
        }
        u.gap(at(600.0));
        for i in 0..4 {
            poll(u, 700.0 + i as f64 * 30.0, "OB DISCHRG", 50.0 - i as f64);
        }
    };
    let mut with = ups();
    let mut without = Ups::builder("ups", "localhost").history(None).build();
    script(&mut with);
    script(&mut without);
    assert!(without.history().is_none());
    for s in (0..1000).step_by(7) {
        assert_eq!(with.estimate(at(s as f64)), without.estimate(at(s as f64)), "at {s}");
    }
    assert!(without.estimate(at(790.0)).is_some());
}

#[test]
fn estimate_uses_only_the_given_time() {
    let mut u = ups();
    discharge(&mut u);
    let e = u.estimate(at(240.0)).unwrap();
    assert_eq!(u.estimate(at(240.0)), Some(e.clone()));
    // Far in the future: the average from 49 % at 60 s on.
    let far = 1e7;
    let e = u.estimate(at(far)).unwrap();
    close(e.per_hour, -3.0 / (far - 60.0) * 3600.0);
    close_opt(e.to_empty, 46.0 * (far - 60.0) / 3.0);
    // Before the last step (even before 1970), the step rate.
    for past in [at(-1e6), at(100.0), UNIX_EPOCH] {
        let e = u.estimate(past).unwrap();
        close(e.per_hour, -60.0);
        close_opt(e.to_low, 26.0 * 60.0);
    }

    // A single step is measured to now, but over a second at least.
    let mut u = ups();
    poll(&mut u, 0.0, "OB DISCHRG", 50.0);
    poll(&mut u, 60.0, "OB DISCHRG", 49.0);
    close(u.estimate(at(far)).unwrap().per_hour, -3600.0 / far);
    close(u.estimate(at(-1e6)).unwrap().per_hour, -3600.0);
    close(u.estimate(at(0.5)).unwrap().per_hour, -3600.0);
}

#[cfg(feature = "demo")]
mod recorded {
    //! The demo's simulated UPSes, polled once a simulated second. The
    //! simulation is accelerated (an outage takes the rack from 100 % to
    //! about 35 % in 105 s) and its load wanders at random, seeded from
    //! the clock, so the checks hold for any run.

    use super::*;
    use upst_core::{Estimate, demo};

    /// What the UPS showed after one frame.
    struct Seen {
        t: f64,
        ob: bool,
        ol: bool,
        lb: bool,
        charge: f64,
        low: f64,
        /// Seconds into the current charge or discharge segment.
        age: f64,
        est: Option<Estimate>,
    }

    fn simulate(name: &str) -> Vec<Vec<(String, String)>> {
        demo::simulate(3000, 1.0).into_iter().find(|s| s.name == name).unwrap().frames
    }

    fn feed(u: &mut Ups, frames: &[Vec<(String, String)>]) -> Vec<Seen> {
        let mut start = 0.0;
        let mut trend = None;
        let mut out = Vec::new();
        for (k, f) in frames.iter().enumerate() {
            let t = k as f64;
            u.apply(f.clone(), at(t));
            if u.tracker().trend() != trend {
                trend = u.tracker().trend();
                start = t;
            }
            let st = u.status();
            out.push(Seen {
                t,
                ob: st.has("OB"),
                ol: st.has("OL"),
                lb: st.has("LB"),
                charge: u.num("battery.charge").unwrap(),
                low: u.num("battery.charge.low").unwrap(),
                age: t - start,
                est: u.estimate(at(t)),
            });
        }
        out
    }

    /// What every estimate must look like, whatever the numbers.
    fn check_shape(s: &Seen) {
        let Some(e) = &s.est else { return };
        assert!(e.per_hour.is_finite(), "at {}: {e:?}", s.t);
        if s.ob {
            assert_eq!(e.trend, Trend::Discharging, "at {}", s.t);
            assert!(e.per_hour < 0.0 && e.to_full.is_none(), "at {}: {e:?}", s.t);
            let empty = e.to_empty.unwrap();
            assert!(empty.is_finite() && empty >= 0.0 && (empty > 0.0) == (s.charge > 0.0), "at {}: {e:?}", s.t);
            match e.to_low {
                Some(low) => assert!(s.charge > s.low && low > 0.0 && low < empty, "at {}: {e:?}", s.t),
                None => assert!(s.charge <= s.low, "at {}: {e:?}", s.t),
            }
        } else {
            assert_eq!(e.trend, Trend::Charging, "at {}", s.t);
            assert!(s.charge < 100.0 && e.per_hour > 0.0, "at {}: {e:?}", s.t);
            assert!(e.to_full.unwrap().is_finite() && e.to_low.is_none() && e.to_empty.is_none(), "at {}", s.t);
        }
    }

    /// Checks the rack's estimates against what happened next; returns
    /// how many predictions of LB and of a full battery were checked.
    fn check_rack(seen: &[Seen]) -> (usize, usize) {
        let (mut lows, mut fulls) = (0, 0);
        let mut outage: Vec<&Seen> = Vec::new();
        let mut charging: Vec<&Seen> = Vec::new();
        for s in seen {
            check_shape(s);
            // The charge drops every second or two on battery.
            if s.ob && s.age >= 5.0 {
                assert!(s.est.is_some(), "no estimate {} s into an outage", s.age);
            }
            if s.ol && s.charge >= 100.0 {
                assert_eq!(s.est, None);
            }

            // LB comes when the charge falls to battery.charge.low: in
            // the last 45 s before it, the prediction is off by at most
            // 3 s (the charge is reported in whole percent) plus 30 % of
            // the time left (the load, and so the rate, wanders). Over
            // 600 runs the worst miss was 1.5 s inside that.
            if !s.ob {
                outage.clear();
            } else if s.lb && !outage.is_empty() {
                for p in outage.drain(..).filter(|p| s.t - p.t <= 45.0) {
                    let e = p.est.as_ref().unwrap();
                    let left = s.t - p.t;
                    let miss = p.t + e.to_low.unwrap() - s.t;
                    assert!(miss.abs() <= 3.0 + 0.3 * left, "LB {left} s ahead predicted {miss:+} s off: {e:?}");
                    lows += 1;
                }
            } else if s.age >= 10.0 && s.est.as_ref().is_some_and(|e| !e.rough && e.to_low.is_some()) {
                outage.push(s);
            }

            // Charging runs at a steady 0.35 %/s: to full within 5 s
            // plus 5 %, the rate within 5 %, once 30 s in.
            if s.ob {
                charging.clear();
            } else if s.charge >= 100.0 {
                for p in charging.drain(..) {
                    let e = p.est.as_ref().unwrap();
                    let left = s.t - p.t;
                    let miss = p.t + e.to_full.unwrap() - s.t;
                    assert!(miss.abs() <= 5.0 + 0.05 * left, "full {left} s ahead predicted {miss:+} s off: {e:?}");
                    fulls += 1;
                }
            } else if let Some(e) = s.est.as_ref().filter(|e| !e.rough && s.age >= 30.0) {
                assert!((e.per_hour / 1260.0 - 1.0).abs() < 0.05, "charging at {} %/h", e.per_hour);
                charging.push(s);
            }
        }
        (lows, fulls)
    }

    #[test]
    fn rack_runs_into_low_battery_and_recharges() {
        // The wandering load decides whether an outage reaches LB and
        // whether the battery recharges to full before the next one; a
        // run of ten outages usually has both, else try another.
        let (mut lows, mut fulls) = (0, 0);
        for _ in 0..20 {
            let mut u = Ups::builder("rack", "demo").history(None).build();
            let (l, f) = check_rack(&feed(&mut u, &simulate("rack")));
            assert!(u.outages() >= 9 && !u.capacity_guessed());
            (lows, fulls) = (lows + l, fulls + f);
            if lows > 0 && fulls > 0 {
                return;
            }
        }
        panic!("20 simulated runs without an outage into LB ({lows}) or a full recharge ({fulls})");
    }

    #[test]
    fn pack_station() {
        let frames = simulate("pack");
        let mut u = Ups::builder("pack", "demo").history(None).build();
        let seen = feed(&mut u, &frames);
        // "Demo Station" is not an EcoFlow: no capacity to guess.
        assert_eq!(u.get("device.mfr"), Some("Demo Station"));
        assert_eq!((u.capacity_wh(), u.capacity_guessed()), (None, false));
        let (mut down, mut up) = (0, 0);
        for s in &seen {
            check_shape(s);
            let Some(e) = &s.est else { continue };
            assert_eq!(e.watts, None);
            if s.ob {
                // OB without DISCHRG; the pack drains 0.12 %/s.
                close_opt(e.to_empty, s.charge / -e.per_hour * 3600.0);
                if !e.rough && s.age >= 40.0 {
                    assert!((e.per_hour / -432.0 - 1.0).abs() < 0.03, "discharging at {} %/h", e.per_hour);
                    down += 1;
                }
            } else {
                // OL without CHRG, below 100 %, charging 0.08 %/s.
                close_opt(e.to_full, (100.0 - s.charge) / e.per_hour * 3600.0);
                if !e.rough && s.age >= 40.0 {
                    assert!((e.per_hour / 288.0 - 1.0).abs() < 0.03, "charging at {} %/h", e.per_hour);
                    up += 1;
                }
            }
        }
        assert!(down > 100 && up > 100, "{down} discharging and {up} charging estimates");

        // The same pack made by EcoFlow: its ups.power.nominal is the
        // pack's 2048 Wh.
        let mfr = |k: &str| k == "device.mfr" || k == "ups.mfr";
        let renamed: Vec<Vec<_>> = frames
            .iter()
            .map(|f| f.iter().map(|(k, v)| (k.clone(), if mfr(k) { "EcoFlow".into() } else { v.clone() })).collect())
            .collect();
        let mut eco = Ups::builder("pack", "demo").history(None).build();
        let eco_seen = feed(&mut eco, &renamed);
        assert_eq!((eco.capacity_wh(), eco.capacity_guessed()), (Some(2048.0), true));
        for (a, b) in seen.iter().zip(&eco_seen) {
            match (&a.est, &b.est) {
                (Some(a), Some(b)) => {
                    let mut without = b.clone();
                    without.watts = None;
                    assert_eq!(without, *a);
                    close_opt(b.watts, b.per_hour.abs() / 100.0 * 2048.0);
                }
                (a, b) => assert_eq!(a, b),
            }
        }
    }
}
