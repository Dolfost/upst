//! Odd numbers a library user can pass in: none may panic.

use std::time::{Duration, UNIX_EPOCH};

use upst_core::history::History;
use upst_core::{Ups, from_unix, unix};

#[test]
fn times_out_of_range_saturate() {
    assert_eq!(from_unix(f64::NAN), UNIX_EPOCH);
    assert_eq!(from_unix(-5.0), UNIX_EPOCH);
    let far = from_unix(f64::INFINITY);
    for t in [1e19, f64::MAX] {
        assert_eq!(from_unix(t), far);
    }
    // Past the year 5000, and still a time every platform holds.
    assert!(unix(far) > 95e9 && far > from_unix(1e10));
    assert_eq!(unix(from_unix(1_700_000_000.5)), 1_700_000_000.5);
}

#[test]
fn histories_of_any_size() {
    for (keep, interval) in [(f64::INFINITY, 2.0), (f64::NAN, 2.0), (3600.0, 0.0), (3600.0, f64::NAN), (-1.0, 1.0)] {
        let mut ups = Ups::builder("ups", "localhost").history(Some(History::new(keep, interval))).build();
        let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for k in 0..3 {
            let vars = vec![("battery.charge".to_string(), (100 - k).to_string())];
            ups.apply(vars, t0 + Duration::from_secs(k * 60));
        }
        assert!(ups.series("battery.charge").is_some_and(|s| !s.is_empty()), "{keep} {interval}");
    }
}

#[test]
fn estimates_at_odd_times() {
    let mut ups = Ups::new("ups", "localhost");
    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    for k in 0..4 {
        let vars = vec![("ups.status".to_string(), "OB".to_string()), ("battery.charge".into(), (90 - k).to_string())];
        ups.apply(vars, t0 + Duration::from_secs(k * 60));
    }
    for at in [UNIX_EPOCH, from_unix(f64::INFINITY), t0 - Duration::from_secs(1)] {
        if let Some(e) = ups.estimate(at) {
            assert!(e.per_hour.is_finite());
        }
    }
}
