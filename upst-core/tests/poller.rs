//! The poller against the demo upsd.

#![cfg(feature = "demo")]

use std::io::Read;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use upst_core::Ups;
use upst_core::demo;
use upst_core::nut::{CmdInfo, VarInfo};
use upst_core::poll::{Action, Config, Creds, Failure, Poller, Update};
use upst_core::target::Server;

const SOON: Duration = Duration::from_secs(5);

fn config(server: &demo::Server) -> Config {
    let mut cfg = Config::new(server.target());
    cfg.interval = Duration::from_millis(150);
    cfg.timeout = Duration::from_secs(2);
    cfg
}

/// An update in a few words, for failure messages.
fn tag(u: &Update) -> String {
    match u {
        Update::Vars { ups, .. } => format!("Vars {ups}"),
        Update::Meta { ups, .. } => format!("Meta {ups}"),
        other => format!("{other:?}"),
    }
}

fn next(rx: &Receiver<Update>) -> Update {
    rx.recv_timeout(SOON).unwrap_or_else(|e| panic!("no update within {SOON:?}: {e}"))
}

/// Skip updates until `pick` takes one.
fn wait<T>(rx: &Receiver<Update>, within: Duration, mut pick: impl FnMut(Update) -> Option<T>) -> T {
    let until = Instant::now() + within;
    let mut skipped = Vec::new();
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let u =
            rx.recv_timeout(left).unwrap_or_else(|e| panic!("nothing matched within {within:?} ({e}): {skipped:?}"));
        let t = tag(&u);
        match pick(u) {
            Some(v) => return v,
            None => skipped.push(t),
        }
    }
}

/// The next poll of `name`.
fn vars_of(rx: &Receiver<Update>, name: &str) -> (Vec<(String, String)>, SystemTime) {
    wait(rx, SOON, |u| match u {
        Update::Vars { ups, vars, at } if ups == name => Some((vars, at)),
        _ => None,
    })
}

fn creds(password: &str) -> Creds {
    Creds { user: "admin".into(), password: password.into() }
}

fn done(rx: &Receiver<Update>) -> (String, Action, Result<String, Failure>) {
    wait(rx, Duration::from_secs(10), |u| match u {
        Update::Done { ups, action, result } => Some((ups, action, result)),
        _ => None,
    })
}

/// Wait out the channel: true once the poller dropped its sender.
fn disconnects(rx: &Receiver<Update>, within: Duration) -> bool {
    let until = Instant::now() + within;
    loop {
        match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(_) => {}
            Err(RecvTimeoutError::Disconnected) => return true,
            Err(RecvTimeoutError::Timeout) => return false,
        }
    }
}

fn eventually(within: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + within;
    while Instant::now() < until {
        if cond() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    cond()
}

/// A port nobody listens on.
fn closed_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn gap(a: SystemTime, b: SystemTime) -> Duration {
    b.duration_since(a).unwrap_or_default()
}

#[test]
fn connects_lists_and_polls() {
    let server = demo::Server::spawn().unwrap();
    let (poller, rx) = Poller::channel(config(&server)).unwrap();
    assert!(matches!(next(&rx), Update::Connecting));
    match next(&rx) {
        Update::Connected { version } => assert_eq!(version.as_deref(), Some("upsd 2.8.4 (upst demo)")),
        other => panic!("{other:?}"),
    }
    match next(&rx) {
        Update::UpsList { list } => assert_eq!(
            list,
            [("rack".to_string(), "Simulated rack UPS".to_string()), ("pack".into(), "Simulated power station".into())]
        ),
        other => panic!("{other:?}"),
    }
    for name in ["rack", "pack"] {
        match next(&rx) {
            Update::Vars { ups, vars, at } => {
                assert_eq!(ups, name);
                assert!(vars.iter().any(|(k, _)| k == "ups.status"), "{vars:?}");
                let now = SystemTime::now();
                let off = now.duration_since(at).unwrap_or_else(|e| e.duration());
                assert!(off < SOON, "{name} polled {off:?} away from now");
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(poller.is_running());
    poller.stop();
}

#[test]
fn names_limit_what_is_polled() {
    let server = demo::Server::spawn().unwrap();
    let mut cfg = config(&server);
    cfg.names = Some(vec!["pack".into(), "ghost".into()]);
    let (poller, rx) = Poller::channel(cfg).unwrap();
    let list = wait(&rx, SOON, |u| match u {
        Update::UpsList { list } => Some(list),
        _ => None,
    });
    assert_eq!(list, [("pack".to_string(), "Simulated power station".to_string()), ("ghost".into(), String::new())]);
    let mut polled = Vec::new();
    while polled.len() < 6 {
        match next(&rx) {
            Update::Vars { ups, .. } => polled.push(ups),
            Update::NoData { ups, error } => {
                assert_eq!((ups.as_str(), error.as_str()), ("ghost", "no such UPS on this server"));
                polled.push(ups);
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(polled, ["pack", "ghost", "pack", "ghost", "pack", "ghost"]);
    poller.stop();
}

#[test]
fn interval_changes_on_the_fly() {
    let server = demo::Server::spawn().unwrap();
    let mut cfg = config(&server);
    cfg.names = Some(vec!["pack".into()]);
    cfg.interval = Duration::from_millis(100);
    let (poller, rx) = Poller::channel(cfg).unwrap();
    let mut times: Vec<SystemTime> = (0..7).map(|_| vars_of(&rx, "pack").1).collect();
    let mut fast: Vec<Duration> = times.windows(2).map(|w| gap(w[0], w[1])).collect();
    fast.sort();

    let slow = Duration::from_millis(700);
    poller.set_interval(slow);
    // Answered after the change took effect: the last poll before it is
    // the one the change triggers.
    poller.describe("pack", "battery.charge");
    let mut anchor = None;
    wait(&rx, SOON, |u| match u {
        Update::Vars { at, .. } => {
            anchor = Some(at);
            None
        }
        Update::Info { .. } => Some(()),
        _ => None,
    });
    times = vec![anchor.expect("set_interval polls at once")];
    times.extend((0..3).map(|_| vars_of(&rx, "pack").1));
    let gaps: Vec<Duration> = times.windows(2).map(|w| gap(w[0], w[1])).collect();

    assert!(fast[fast.len() / 2] < Duration::from_millis(350), "at 100 ms: {fast:?}");
    assert!(gaps.iter().all(|g| *g >= Duration::from_millis(650)), "at 700 ms: {gaps:?}");
    poller.stop();
}

#[test]
fn pause_and_resume() {
    let server = demo::Server::spawn().unwrap();
    let mut cfg = config(&server);
    cfg.interval = Duration::from_millis(100);
    let (poller, rx) = Poller::channel(cfg).unwrap();
    vars_of(&rx, "pack");
    poller.pause(true);
    // A barrier: the Info comes after the pause took effect.
    poller.describe("rack", "ups.status");
    wait(&rx, SOON, |u| matches!(u, Update::Info { .. }).then_some(()));
    let quiet = Instant::now() + Duration::from_millis(800);
    while let Ok(u) = rx.recv_timeout(quiet.saturating_duration_since(Instant::now())) {
        assert!(!matches!(u, Update::Vars { .. }), "polled while paused: {}", tag(&u));
    }
    let t = Instant::now();
    poller.pause(false);
    vars_of(&rx, "rack");
    assert!(t.elapsed() < Duration::from_secs(2), "resumed after {:?}", t.elapsed());
    poller.stop();
}

#[test]
fn refresh_polls_at_once() {
    let server = demo::Server::spawn().unwrap();
    let mut cfg = config(&server);
    cfg.interval = Duration::from_secs(60);
    let (poller, rx) = Poller::channel(cfg).unwrap();
    vars_of(&rx, "pack");
    for _ in 0..2 {
        let t = Instant::now();
        poller.refresh();
        vars_of(&rx, "rack");
        vars_of(&rx, "pack");
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    }
    poller.stop();
}

#[test]
fn describe_reports_what_upsd_knows() {
    let server = demo::Server::spawn().unwrap();
    let (poller, rx) = Poller::channel(config(&server)).unwrap();
    vars_of(&rx, "rack");
    let info = |rx: &Receiver<Update>, var: &str| {
        poller.describe("rack", var);
        wait(rx, SOON, |u| match u {
            Update::Info { ups, var: v, info } if ups == "rack" && v == var => Some(info),
            _ => None,
        })
    };
    assert_eq!(
        info(&rx, "input.sensitivity"),
        VarInfo {
            desc: Some("Input power sensitivity".into()),
            kind: vec!["RW".into(), "ENUM".into(), "STRING:32".into()],
            enum_values: vec!["low".into(), "medium".into(), "high".into()],
            ranges: vec![],
        }
    );
    assert_eq!(info(&rx, "ups.delay.start").ranges, [("0".to_string(), "600".to_string())]);
    assert_eq!(info(&rx, "no.such.var"), VarInfo::default());
    poller.stop();
}

#[test]
fn runs_actions_with_credentials() {
    let server = demo::Server::spawn().unwrap();
    let (poller, rx) = Poller::channel(config(&server)).unwrap();
    vars_of(&rx, "rack");
    let mute = Action::Command { cmd: "beeper.mute".into(), value: None };

    poller.run("rack", mute.clone(), creds("s3cret"));
    let (ups, action, result) = done(&rx);
    assert_eq!((ups.as_str(), &action), ("rack", &mute));
    assert_eq!(result, Ok("done".to_string()));

    poller.run("rack", mute.clone(), creds("wrong"));
    match done(&rx).2 {
        Err(Failure { message, auth: true }) => assert!(message.starts_with("access denied"), "{message}"),
        other => panic!("{other:?}"),
    }

    poller.run("rack", Action::Command { cmd: "nope".into(), value: None }, creds("s3cret"));
    assert_eq!(done(&rx).2, Err(Failure { message: "command not supported".into(), auth: false }));

    let set = Action::Set { var: "input.sensitivity".into(), value: "high".into() };
    poller.run("rack", set.clone(), creds("s3cret"));
    let (_, action, result) = done(&rx);
    assert_eq!((action, result), (set, Ok("done".to_string())));

    poller.refresh();
    let (vars, _) = vars_of(&rx, "rack");
    let get = |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    assert_eq!(get("ups.beeper.status"), Some("muted"));
    assert_eq!(get("input.sensitivity"), Some("high"));
    poller.stop();
}

#[test]
fn meta_lists_commands_and_writable_vars() {
    let server = demo::Server::spawn().unwrap();
    let mut cfg = config(&server);
    cfg.meta = true;
    let (poller, rx) = Poller::channel(cfg).unwrap();
    let mut metas = Vec::new();
    // Both come before the first poll.
    while metas.len() < 2 {
        match next(&rx) {
            Update::Meta { ups, cmds, rw } => metas.push((ups, cmds, rw)),
            Update::Vars { ups, .. } => panic!("polled {ups} before every Meta"),
            _ => {}
        }
    }
    let (ups, cmds, rw) = &metas[0];
    assert_eq!(ups, "rack");
    assert_eq!(cmds.len(), 11);
    assert!(
        cmds.contains(&CmdInfo { name: "beeper.mute".into(), desc: Some("Temporarily mute the UPS beeper".into()) })
    );
    assert!(cmds.iter().all(|c| c.desc.is_some()), "{cmds:?}");
    assert_eq!(rw.len(), 8);
    assert!(rw.iter().any(|v| v == "input.sensitivity"));
    let (ups, cmds, rw) = &metas[1];
    assert_eq!(ups, "pack");
    assert_eq!(
        cmds,
        &[CmdInfo { name: "driver.reload".into(), desc: Some("Reload running driver configuration".into()) }]
    );
    assert!(rw.is_empty());
    poller.stop();
}

#[test]
fn a_callback_returning_false_stops_the_thread() {
    let server = demo::Server::spawn().unwrap();
    let (said_no, late) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
    let (no, l) = (said_no.clone(), late.clone());
    let poller = Poller::spawn(config(&server), move |u| {
        if no.load(Ordering::SeqCst) {
            l.fetch_add(1, Ordering::SeqCst);
        }
        if matches!(u, Update::Vars { .. }) {
            no.store(true, Ordering::SeqCst);
            return false;
        }
        true
    })
    .unwrap();
    assert!(eventually(SOON, || !poller.is_running()), "still running");
    assert!(said_no.load(Ordering::SeqCst));
    // Requests to a stopped poller go nowhere.
    poller.refresh();
    poller.describe("rack", "ups.status");
    thread::sleep(Duration::from_millis(200));
    assert_eq!(late.load(Ordering::SeqCst), 0, "called after saying no");
    poller.stop();
}

#[test]
fn dropping_the_receiver_stops_the_thread() {
    let server = demo::Server::spawn().unwrap();
    let mut cfg = config(&server);
    cfg.interval = Duration::from_millis(50);
    let (poller, rx) = Poller::channel(cfg).unwrap();
    vars_of(&rx, "rack");
    drop(rx);
    assert!(eventually(SOON, || !poller.is_running()), "still running");
}

#[test]
fn stop_returns_quickly_and_ends_the_thread() {
    let server = demo::Server::spawn().unwrap();
    let mut cfg = config(&server);
    cfg.interval = Duration::from_secs(60);
    // The callback holds this until the thread lets go of it.
    let token = Arc::new(());
    let held = token.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let poller = Poller::spawn(cfg, move |u| {
        let _ = &held;
        s.lock().unwrap().push(tag(&u));
        true
    })
    .unwrap();
    assert!(eventually(SOON, || seen.lock().unwrap().iter().any(|t| t == "Vars pack")));
    let t = Instant::now();
    poller.stop();
    assert!(t.elapsed() < Duration::from_secs(2), "stop took {:?}", t.elapsed());
    assert_eq!(Arc::strong_count(&token), 1, "the thread still holds the callback");
}

#[test]
fn dropping_the_poller_stops_it() {
    let server = demo::Server::spawn().unwrap();
    let (poller, rx) = Poller::channel(config(&server)).unwrap();
    vars_of(&rx, "rack");
    drop(poller);
    assert!(disconnects(&rx, SOON), "the poller thread lives on");
}

#[test]
fn stop_while_down_returns_quickly() {
    let mut cfg = Config::new(Server { host: "127.0.0.1".into(), port: closed_port() });
    cfg.timeout = Duration::from_secs(2);
    let (poller, rx) = Poller::channel(cfg).unwrap();
    wait(&rx, SOON, |u| matches!(u, Update::Down { .. }).then_some(()));
    let t = Instant::now();
    poller.stop();
    assert!(t.elapsed() < Duration::from_millis(500), "stop took {:?}", t.elapsed());
    assert!(disconnects(&rx, Duration::from_millis(100)));
}

/// A server that accepts and then never says a word.
fn silent_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
            let _ = s.read_to_end(&mut Vec::new());
        }
    });
    port
}

#[test]
fn stop_waits_for_a_network_call_in_flight() {
    let timeout = Duration::from_millis(800);
    let mut cfg = Config::new(Server { host: "127.0.0.1".into(), port: silent_server() });
    cfg.timeout = timeout;
    let (poller, rx) = Poller::channel(cfg).unwrap();
    assert!(matches!(next(&rx), Update::Connecting));
    // The poller now waits for upsd to answer VER.
    let t = Instant::now();
    poller.stop();
    let took = t.elapsed();
    assert!(took < timeout + Duration::from_secs(1), "stop took {took:?}");
    assert!(disconnects(&rx, Duration::from_millis(100)), "updates after stop");
}

#[test]
fn a_silent_server_is_down_without_connecting() {
    let mut cfg = Config::new(Server { host: "127.0.0.1".into(), port: silent_server() });
    cfg.timeout = Duration::from_millis(500);
    let (poller, rx) = Poller::channel(cfg).unwrap();
    assert!(matches!(next(&rx), Update::Connecting));
    match next(&rx) {
        Update::Down { error, .. } => assert_eq!(error, "timed out"),
        other => panic!("{}", tag(&other)),
    }
    poller.stop();
}

#[test]
fn reconnects_with_backoff_when_upsd_goes_away() {
    let server = demo::Server::spawn().unwrap();
    let mut cfg = config(&server);
    cfg.interval = Duration::from_millis(100);
    let (poller, rx) = Poller::channel(cfg).unwrap();
    vars_of(&rx, "pack");
    server.stop();
    let mut retries = Vec::new();
    while retries.len() < 2 {
        match next(&rx) {
            Update::Down { retry, .. } => retries.push(retry),
            Update::Connected { .. } => panic!("connected to a stopped server"),
            _ => {}
        }
    }
    assert_eq!(retries, [Duration::from_secs(1), Duration::from_secs(2)]);
    poller.stop();
}

#[test]
fn a_closed_port_is_down_from_the_start() {
    let cfg = Config::new(Server { host: "127.0.0.1".into(), port: closed_port() });
    let (poller, rx) = Poller::channel(cfg).unwrap();
    let connecting = |rx: &Receiver<Update>| match next(rx) {
        Update::Connecting => {}
        other => panic!("{}", tag(&other)),
    };
    let down = |rx: &Receiver<Update>, secs: u64| match next(rx) {
        Update::Down { error, retry } => {
            assert_eq!(error, "connection refused");
            assert_eq!(retry, Duration::from_secs(secs));
        }
        other => panic!("{}", tag(&other)),
    };
    connecting(&rx);
    down(&rx, 1);
    connecting(&rx);
    down(&rx, 2);
    // A refresh skips the rest of the wait.
    let t = Instant::now();
    poller.refresh();
    connecting(&rx);
    assert!(t.elapsed() < Duration::from_secs(1), "refresh took {:?}", t.elapsed());
    down(&rx, 4);
    poller.stop();
}

#[test]
fn vars_feed_a_live_ups() {
    let server = demo::Server::spawn().unwrap();
    let (poller, rx) = Poller::channel(config(&server)).unwrap();
    let label = server.target().to_string();
    let mut ups = Ups::builder("rack", &label).history(None).build();
    assert!(!ups.is_live());
    let (vars, at) = vars_of(&rx, "rack");
    ups.apply(vars, at);
    assert!(ups.is_live());
    assert_eq!(ups.updated(), Some(at));
    assert_eq!(ups.label(), format!("rack@{label}"));
    let charge = ups.num("battery.charge").expect("a charge");
    assert!((0.0..=100.0).contains(&charge), "{charge}");
    assert!(ups.status().has("OL") || ups.status().has("OB"), "{:?}", ups.get("ups.status"));
    let (vars, at) = vars_of(&rx, "rack");
    ups.apply(vars, at);
    assert_eq!(ups.polls(), 2);
    poller.stop();
}

#[test]
fn an_interval_too_long_to_count_waits_for_requests() {
    let server = demo::Server::spawn().unwrap();
    let mut cfg = config(&server);
    cfg.names = Some(vec!["pack".into()]);
    cfg.interval = Duration::MAX;
    let (poller, rx) = Poller::channel(cfg).unwrap();
    vars_of(&rx, "pack");
    poller.refresh();
    vars_of(&rx, "pack");
    poller.set_interval(Duration::MAX);
    vars_of(&rx, "pack");
    assert!(poller.is_running());
    let t = Instant::now();
    poller.stop();
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
}

/// The poller, for its own callback to stop.
type Slot = Arc<Mutex<Option<Poller>>>;

/// Wait for the poller's thread to end, which drops the callback and so
/// `rx`'s sender; anything else that comes is skipped.
fn ends(rx: &Receiver<()>, within: Duration) -> bool {
    let until = Instant::now() + within;
    loop {
        match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(()) => {}
            Err(RecvTimeoutError::Disconnected) => return true,
            Err(RecvTimeoutError::Timeout) => return false,
        }
    }
}

#[test]
fn stop_from_the_callback_ends_the_thread() {
    let server = demo::Server::spawn().unwrap();
    let slot = Slot::default();
    let (tx, rx) = std::sync::mpsc::channel();
    let s = slot.clone();
    let poller = Poller::spawn(config(&server), move |u| {
        if matches!(u, Update::Vars { .. }) {
            if let Some(p) = s.lock().unwrap().take() {
                p.stop();
            }
            let _ = tx.send(());
        }
        true
    })
    .unwrap();
    *slot.lock().unwrap() = Some(poller);
    assert!(ends(&rx, SOON));
}

#[test]
fn stop_from_an_action_callback_does_not_deadlock() {
    let server = demo::Server::spawn().unwrap();
    let mut cfg = config(&server);
    cfg.interval = Duration::from_millis(20);
    let slot = Slot::default();
    let (tx, rx) = std::sync::mpsc::channel();
    let s = slot.clone();
    let poller = Poller::spawn(cfg, move |u| {
        if matches!(u, Update::Done { .. }) {
            // Long enough for the poller to want the callback meanwhile.
            thread::sleep(Duration::from_millis(300));
            if let Some(p) = s.lock().unwrap().take() {
                p.stop();
            }
            let _ = tx.send(());
        }
        true
    })
    .unwrap();
    poller.run("rack", Action::Command { cmd: "beeper.mute".into(), value: None }, creds("secret"));
    *slot.lock().unwrap() = Some(poller);
    assert!(ends(&rx, Duration::from_secs(10)));
}

#[test]
fn false_from_an_action_callback_stops_a_paused_poller() {
    let server = demo::Server::spawn().unwrap();
    let mut cfg = config(&server);
    cfg.names = Some(vec!["pack".into()]);
    cfg.interval = Duration::from_secs(3600);
    let (tx, rx) = std::sync::mpsc::channel();
    let poller = Poller::spawn(cfg, move |u| {
        let done = matches!(u, Update::Done { .. });
        let _ = tx.send(u);
        !done
    })
    .unwrap();
    vars_of(&rx, "pack");
    poller.pause(true);
    poller.run("pack", Action::Command { cmd: "driver.reload".into(), value: None }, creds("secret"));
    let (ups, _, result) = done(&rx);
    assert_eq!(ups, "pack");
    assert!(result.is_ok(), "{result:?}");
    let until = Instant::now() + Duration::from_secs(2);
    while poller.is_running() && Instant::now() < until {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!poller.is_running());
}
