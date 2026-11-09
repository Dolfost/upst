//! `--demo`: a simulated upsd on a loopback port, so upst can be tried
//! (and tested) without a UPS. It speaks the same protocol as the real
//! one and serves two UPSes on an accelerated clock:
//!
//! - `rack`: a line-interactive UPS with every common variable, an outage
//!   every few minutes that runs into low battery, writable settings and
//!   instant commands with tracking.
//! - `pack`: a portable power station like the EcoFlow that only reports
//!   charge, runtime and battery voltage, with its own outage cycle.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::nut::{quote, tokenize};
use crate::target::Server;

/// Polls of one simulated UPS: (name, [(unix time, variables)]).
pub type Past = Vec<(String, Vec<(f64, Vec<(String, String)>)>)>;

/// Start the server; returns where it listens.
pub fn start() -> io::Result<Server> {
    serve_sim(Sim::new())
}

/// Start the server with a simulation that has already run for `secs`,
/// and return the polls of that time, `step` seconds apart, for a
/// recorder to hold.
pub fn start_with_history(secs: f64, step: f64) -> io::Result<(Server, Past)> {
    let mut sim = Sim::new();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64()).floor();
    let n = (secs / step) as usize;
    let mut past: Past = sim.units.iter().map(|u| (u.name.to_string(), Vec::with_capacity(n))).collect();
    for k in 0..n {
        sim.step(step);
        let t = now - (n - k) as f64 * step;
        for (i, u) in sim.units.iter().enumerate() {
            past[i].1.push((t, u.vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect()));
        }
    }
    sim.last = Instant::now();
    Ok((serve_sim(sim)?, past))
}

fn serve_sim(sim: Sim) -> io::Result<Server> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let sim = Arc::new(Mutex::new(sim));
    thread::Builder::new().name("upst demo".into()).spawn(move || {
        for conn in listener.incoming().flatten() {
            let sim = sim.clone();
            thread::spawn(move || {
                let _ = serve(conn, sim);
            });
        }
    })?;
    Ok(Server { host: "127.0.0.1".into(), port })
}

/// xorshift; the demo needs wobble, not cryptography.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [-1, 1).
    fn wobble(&mut self) -> f64 {
        self.next() * 2.0 - 1.0
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Rack,
    Pack,
}

struct Unit {
    name: &'static str,
    desc: &'static str,
    kind: Kind,
    vars: BTreeMap<String, String>,
    rw: Vec<&'static str>,
    enums: Vec<(&'static str, Vec<&'static str>)>,
    ranges: Vec<(&'static str, (i64, i64))>,
    cmds: Vec<(&'static str, &'static str)>,
    /// Simulation state.
    charge: f64,
    load: f64,
    vin: f64,
    on_battery: bool,
    output_off_until: Option<Instant>,
    test_until: Option<Instant>,
    cal_until: Option<Instant>,
    /// Seconds into the outage cycle, and the cycle length.
    phase: f64,
    cycle: (f64, f64, f64),
}

impl Unit {
    fn set(&mut self, k: &str, v: impl ToString) {
        self.vars.insert(k.to_string(), v.to_string());
    }

    fn get_num(&self, k: &str) -> f64 {
        self.vars.get(k).and_then(|v| v.parse().ok()).unwrap_or(0.0)
    }
}

fn rack() -> Unit {
    let fixed: &[(&str, &str)] = &[
        ("battery.charge.low", "40"),
        ("battery.charge.warning", "55"),
        ("battery.date", "2024/03/15"),
        ("battery.mfr.date", "2024/02/02"),
        ("battery.packs", "1"),
        ("battery.runtime.low", "180"),
        ("battery.type", "PbAc"),
        ("battery.voltage.nominal", "24.0"),
        ("device.mfr", "Demo Power"),
        ("device.model", "Smart 1500 RM (simulated)"),
        ("device.serial", "DEMO-0001"),
        ("device.type", "ups"),
        ("driver.name", "usbhid-ups"),
        ("driver.parameter.pollfreq", "30"),
        ("driver.parameter.pollinterval", "2"),
        ("driver.parameter.port", "auto"),
        ("driver.state", "quiet"),
        ("driver.version", "2.8.4"),
        ("driver.version.data", "Demo HID 0.1"),
        ("input.frequency.nominal", "50"),
        ("input.sensitivity", "medium"),
        ("input.transfer.high", "253"),
        ("input.transfer.low", "200"),
        ("input.voltage.nominal", "230"),
        ("output.voltage.nominal", "230"),
        ("ups.beeper.status", "enabled"),
        ("ups.delay.shutdown", "20"),
        ("ups.delay.start", "30"),
        ("ups.firmware", "UPS 09.3 / ID=18"),
        ("ups.mfr", "Demo Power"),
        ("ups.model", "Smart 1500 RM (simulated)"),
        ("ups.power.nominal", "1500"),
        ("ups.productid", "0003"),
        ("ups.realpower.nominal", "1000"),
        ("ups.serial", "DEMO-0001"),
        ("ups.test.result", "No test initiated"),
        ("ups.timer.shutdown", "-1"),
        ("ups.timer.start", "-1"),
        ("ups.vendorid", "051d"),
    ];
    Unit {
        name: "rack",
        desc: "Simulated rack UPS",
        kind: Kind::Rack,
        vars: fixed.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        rw: vec![
            "battery.charge.low",
            "battery.runtime.low",
            "input.sensitivity",
            "input.transfer.high",
            "input.transfer.low",
            "ups.beeper.status",
            "ups.delay.shutdown",
            "ups.delay.start",
        ],
        enums: vec![
            ("input.sensitivity", vec!["low", "medium", "high"]),
            ("ups.beeper.status", vec!["enabled", "disabled", "muted"]),
        ],
        ranges: vec![
            ("battery.charge.low", (10, 90)),
            ("battery.runtime.low", (60, 1800)),
            ("input.transfer.high", (245, 265)),
            ("input.transfer.low", (160, 215)),
            ("ups.delay.shutdown", (0, 600)),
            ("ups.delay.start", (0, 600)),
        ],
        cmds: vec![
            ("beeper.disable", "Disable the UPS beeper"),
            ("beeper.enable", "Enable the UPS beeper"),
            ("beeper.mute", "Temporarily mute the UPS beeper"),
            ("calibrate.start", "Start runtime calibration"),
            ("calibrate.stop", "Stop runtime calibration"),
            ("load.off", "Turn off the load immediately"),
            ("load.on", "Turn on the load immediately"),
            ("shutdown.return", "Turn off the load and return when power is back"),
            ("test.battery.start.deep", "Start a deep battery test"),
            ("test.battery.start.quick", "Start a quick battery test"),
            ("test.battery.stop", "Stop the battery test"),
        ],
        charge: 100.0,
        load: 31.0,
        vin: 230.0,
        on_battery: false,
        output_off_until: None,
        test_until: None,
        cal_until: None,
        phase: 25.0,
        cycle: (45.0, 150.0, 300.0),
    }
}

fn pack() -> Unit {
    let fixed: &[(&str, &str)] = &[
        ("battery.capacity", "100"),
        ("battery.capacity.nominal", "100"),
        ("battery.charge.low", "20"),
        ("battery.charge.warning", "5"),
        ("battery.runtime.low", "300"),
        ("battery.type", ""),
        ("battery.voltage.nominal", "13.8"),
        ("device.mfr", "Demo Station"),
        ("device.model", "PowerPack 2000 (simulated)"),
        ("device.serial", "DEMO-0002"),
        ("device.type", "ups"),
        ("driver.flag.pollonly", "enabled"),
        ("driver.name", "usbhid-ups"),
        ("driver.parameter.pollfreq", "30"),
        ("driver.parameter.pollinterval", "2"),
        ("driver.parameter.port", "auto"),
        ("driver.state", "quiet"),
        ("driver.version", "2.8.4"),
        ("driver.version.data", "Demo HID 0.1"),
        ("ups.beeper.status", "enabled"),
        ("ups.mfr", "Demo Station"),
        ("ups.model", "PowerPack 2000 (simulated)"),
        ("ups.power.nominal", "2048"),
        ("ups.serial", "DEMO-0002"),
    ];
    Unit {
        name: "pack",
        desc: "Simulated power station",
        kind: Kind::Pack,
        vars: fixed.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        rw: vec![],
        enums: vec![],
        ranges: vec![],
        cmds: vec![("driver.reload", "Reload running driver configuration")],
        charge: 87.0,
        load: 0.0,
        vin: 0.0,
        on_battery: false,
        output_off_until: None,
        test_until: None,
        cal_until: None,
        phase: 0.0,
        cycle: (120.0, 300.0, 420.0),
    }
}

struct Sim {
    units: Vec<Unit>,
    last: Instant,
    rng: Rng,
    tracking: Vec<(String, Instant, bool)>,
}

impl Sim {
    fn new() -> Self {
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
        let mut sim = Sim { units: vec![rack(), pack()], last: Instant::now(), rng: Rng(seed), tracking: Vec::new() };
        sim.step(0.0);
        sim
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        if dt >= 0.2 {
            self.last = now;
            self.step(dt.min(10.0));
        }
    }

    fn step(&mut self, dt: f64) {
        let now = Instant::now();
        for i in 0..self.units.len() {
            let mut w = || self.rng.wobble();
            let u = &mut self.units[i];
            u.phase = (u.phase + dt) % u.cycle.2;
            let (start, end, _) = u.cycle;
            u.on_battery = u.phase >= start && u.phase < end;
            let off = u.output_off_until.is_some_and(|t| now < t);
            if !off {
                u.output_off_until = None;
            }
            match u.kind {
                Kind::Rack => step_rack(u, dt, off, now, &mut w),
                Kind::Pack => step_pack(u, dt, &mut w),
            }
        }
    }
}

fn step_rack(u: &mut Unit, dt: f64, off: bool, now: Instant, w: &mut impl FnMut() -> f64) {
    // A wandering load, mostly 25-40 %.
    u.load = (u.load + w() * 1.5 * dt.sqrt()).clamp(18.0, 48.0);
    let load = if off { 0.0 } else { u.load };
    let testing = u.test_until.is_some_and(|t| now < t);
    let calibrating = u.cal_until.is_some_and(|t| now < t);
    let discharging = u.on_battery || testing || calibrating;
    if discharging {
        // Accelerated: about 100 % -> 30 % across one simulated outage.
        u.charge -= dt * (0.35 + load / 100.0 * 0.9);
    } else if u.charge < 100.0 {
        u.charge += dt * 0.35;
    }
    u.charge = u.charge.clamp(0.0, 100.0);
    if u.on_battery {
        u.vin = 0.0;
    } else {
        // Mains around 230 V with an occasional sag.
        let sag = if (u.phase % 97.0) < 9.0 { -24.0 } else { 0.0 };
        let target = 231.0 + sag;
        u.vin = if u.vin < 150.0 { target } else { u.vin + (target - u.vin) * 0.5 + w() * 0.8 };
    }
    let low = u.get_num("battery.charge.low");
    let runtime = u.charge / 100.0 * 2700.0 * (30.0 / load.max(5.0));
    let mut status = vec![if u.on_battery { "OB" } else { "OL" }];
    if discharging {
        status.push("DISCHRG");
    } else if u.charge < 99.5 {
        status.push("CHRG");
    }
    if !u.on_battery && u.vin > 0.0 && u.vin < u.get_num("input.transfer.low") + 12.0 {
        status.push("BOOST");
    }
    if u.charge <= low || runtime <= u.get_num("battery.runtime.low") {
        status.push("LB");
    }
    if calibrating {
        status.push("CAL");
    }
    if off {
        status.push("OFF");
    }
    if let Some(t) = u.test_until
        && now >= t
    {
        u.test_until = None;
        u.set("ups.test.result", "Done and passed");
    }
    let vout = if off {
        0.0
    } else if u.on_battery {
        230.0 + w() * 0.3
    } else {
        230.0 + w() * 0.6
    };
    let vbat = 24.0 + u.charge / 100.0 * 3.2 - if discharging { 1.1 + load / 100.0 } else { -0.4 };
    let temp = u.get_num("battery.temperature");
    let temp = if temp == 0.0 { 27.0 } else { temp + (27.5 - temp) * 0.02 + w() * 0.05 };
    u.set("battery.charge", format!("{:.0}", u.charge.floor()));
    u.set("battery.runtime", format!("{:.0}", runtime));
    u.set("battery.voltage", format!("{vbat:.1}"));
    u.set("battery.temperature", format!("{temp:.1}"));
    u.set("input.voltage", format!("{:.1}", u.vin));
    u.set("input.frequency", if u.on_battery { "0.0".to_string() } else { format!("{:.1}", 50.0 + w() * 0.05) });
    u.set("output.voltage", format!("{vout:.1}"));
    u.set("output.frequency", format!("{:.1}", 50.0 + w() * 0.02));
    u.set("output.current", format!("{:.2}", load / 100.0 * 1000.0 / 230.0));
    u.set("ups.load", format!("{load:.0}"));
    u.set("ups.realpower", format!("{:.0}", load / 100.0 * 1000.0));
    u.set("ups.power", format!("{:.0}", load / 100.0 * 1000.0 / 0.82));
    u.set("ups.temperature", format!("{:.1}", 31.0 + load / 20.0));
    u.set("ambient.temperature", format!("{:.1}", 23.4 + w() * 0.1));
    u.set("ambient.humidity", format!("{:.0}", 41.0 + w()));
    u.set("ups.status", status.join(" "));
    if u.on_battery {
        u.set("input.transfer.reason", "input voltage out of range");
    }
}

fn step_pack(u: &mut Unit, dt: f64, w: &mut impl FnMut() -> f64) {
    if u.on_battery {
        u.charge -= dt * 0.12;
    } else if u.charge < 100.0 {
        u.charge += dt * 0.08;
    }
    u.charge = u.charge.clamp(0.0, 100.0);
    let charge = u.charge.floor();
    // The station reports whole minutes and caps them.
    let minutes = if u.on_battery { (charge / 100.0 * 2048.0 / 140.0 * 60.0).round() } else { 7998.0 };
    let mut status = vec![if u.on_battery { "OB" } else { "OL" }];
    if charge <= u.get_num("battery.charge.low") {
        status.push("LB");
    }
    let v = 12.4 + charge / 100.0 * 0.7 + if u.on_battery { -0.2 } else { 0.1 } + w() * 0.02;
    u.set("battery.charge", format!("{charge:.0}"));
    u.set("battery.runtime", format!("{:.0}", minutes * 60.0));
    u.set("battery.voltage", format!("{v:.1}"));
    u.set("ups.status", status.join(" "));
}

fn serve(conn: TcpStream, sim: Arc<Mutex<Sim>>) -> io::Result<()> {
    let mut out = conn.try_clone()?;
    let reader = BufReader::new(conn);
    let (mut user, mut pass) = (None::<String>, None::<String>);
    let mut tracking = false;
    for line in reader.lines() {
        let line = line?;
        let words = tokenize(&line);
        let w: Vec<&str> = words.iter().map(String::as_str).collect();
        let mut sim = sim.lock().unwrap();
        sim.tick();
        let reply = match w.as_slice() {
            ["LOGOUT"] => {
                out.write_all(b"OK Goodbye\n")?;
                return Ok(());
            }
            ["VER"] => "Network UPS Tools upsd 2.8.4 (upst demo) - https://networkupstools.org/".into(),
            ["NETVER"] | ["PROTVER"] => "1.3".into(),
            ["USERNAME", u] => {
                user = Some(u.to_string());
                "OK".into()
            }
            ["PASSWORD", p] => {
                pass = Some(p.to_string());
                "OK".into()
            }
            ["SET", "TRACKING", "ON"] if user.is_some() => {
                tracking = true;
                "OK".into()
            }
            ["SET", "TRACKING", _] => "ERR USERNAME-REQUIRED".into(),
            ["GET", "TRACKING", id] => match sim.tracking.iter().find(|t| t.0 == *id) {
                Some((_, at, _)) if at.elapsed() < Duration::from_millis(800) => "PENDING".into(),
                Some((_, _, true)) => "SUCCESS".into(),
                Some((_, _, false)) => "ERR INSTCMD-FAILED".into(),
                None => "ERR UNKNOWN".into(),
            },
            ["LIST", "UPS"] => {
                let mut s = "BEGIN LIST UPS\n".to_string();
                for u in &sim.units {
                    s += &format!("UPS {} {}\n", u.name, quote(u.desc));
                }
                s + "END LIST UPS"
            }
            ["LIST", kind @ ("VAR" | "RW" | "CMD"), name] => match sim.units.iter().find(|u| u.name == *name) {
                None => "ERR UNKNOWN-UPS".into(),
                Some(u) => {
                    let mut s = format!("BEGIN LIST {kind} {name}\n");
                    match *kind {
                        "VAR" => {
                            for (k, v) in &u.vars {
                                s += &format!("VAR {name} {k} {}\n", quote_always(v));
                            }
                        }
                        "RW" => {
                            for k in &u.rw {
                                let v = u.vars.get(*k).map_or("", String::as_str);
                                s += &format!("RW {name} {k} {}\n", quote_always(v));
                            }
                        }
                        _ => {
                            for (c, _) in &u.cmds {
                                s += &format!("CMD {name} {c}\n");
                            }
                        }
                    }
                    s + &format!("END LIST {kind} {name}")
                }
            },
            ["LIST", kind @ ("ENUM" | "RANGE"), name, var] => match sim.units.iter().find(|u| u.name == *name) {
                None => "ERR UNKNOWN-UPS".into(),
                Some(u) => {
                    let mut s = format!("BEGIN LIST {kind} {name} {var}\n");
                    if *kind == "ENUM" {
                        for (_, vals) in u.enums.iter().filter(|e| e.0 == *var) {
                            for v in vals {
                                s += &format!("ENUM {name} {var} {}\n", quote_always(v));
                            }
                        }
                    } else {
                        for (_, (lo, hi)) in u.ranges.iter().filter(|r| r.0 == *var) {
                            s += &format!("RANGE {name} {var} \"{lo}\" \"{hi}\"\n");
                        }
                    }
                    s + &format!("END LIST {kind} {name} {var}")
                }
            },
            ["GET", what @ ("VAR" | "DESC" | "TYPE" | "CMDDESC"), name, item] => {
                match sim.units.iter().find(|u| u.name == *name) {
                    None => "ERR UNKNOWN-UPS".into(),
                    Some(u) => get(u, what, item),
                }
            }
            ["GET", "UPSDESC", name] => match sim.units.iter().find(|u| u.name == *name) {
                None => "ERR UNKNOWN-UPS".into(),
                Some(u) => format!("UPSDESC {name} {}", quote(u.desc)),
            },
            ["INSTCMD", name, cmd, ..] | ["SET", "VAR", name, cmd, ..] => {
                let set = w[0] == "SET";
                let value = if set { w.get(4).copied() } else { w.get(3).copied() };
                match (&user, &pass) {
                    (None, _) => "ERR USERNAME-REQUIRED".into(),
                    (_, None) => "ERR PASSWORD-REQUIRED".into(),
                    // A way to see the failure path in the demo.
                    (_, Some(p)) if p == "wrong" => "ERR ACCESS-DENIED".into(),
                    _ => {
                        let r = if set {
                            set_var(&mut sim, name, cmd, value.unwrap_or(""))
                        } else {
                            instcmd(&mut sim, name, cmd)
                        };
                        match r {
                            Err(code) => format!("ERR {code}"),
                            Ok(()) if tracking => {
                                let id = format!("{:016x}", (sim.rng.next() * u64::MAX as f64) as u64);
                                sim.tracking.push((id.clone(), Instant::now(), true));
                                format!("OK TRACKING {id}")
                            }
                            Ok(()) => "OK".into(),
                        }
                    }
                }
            }
            ["HELP"] => "Commands: HELP VER GET LIST SET INSTCMD LOGOUT USERNAME PASSWORD".into(),
            [] => continue,
            _ => "ERR UNKNOWN-COMMAND".into(),
        };
        drop(sim);
        out.write_all(reply.as_bytes())?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

fn quote_always(v: &str) -> String {
    let q = quote(v);
    if q.starts_with('"') { q } else { format!("\"{q}\"") }
}

fn get(u: &Unit, what: &str, item: &str) -> String {
    let name = u.name;
    match what {
        "VAR" => match u.vars.get(item) {
            Some(v) => format!("VAR {name} {item} {}", quote_always(v)),
            None => "ERR VAR-NOT-SUPPORTED".into(),
        },
        "DESC" => {
            let d = var_desc(item);
            format!("DESC {name} {item} {}", quote_always(d))
        }
        "CMDDESC" => match u.cmds.iter().find(|c| c.0 == item) {
            Some((_, d)) => format!("CMDDESC {name} {item} {}", quote_always(d)),
            None => "ERR CMD-NOT-SUPPORTED".into(),
        },
        _ => {
            if !u.vars.contains_key(item) {
                return "ERR VAR-NOT-SUPPORTED".into();
            }
            let mut kind = Vec::new();
            if u.rw.contains(&item) {
                kind.push("RW".to_string());
            }
            if u.enums.iter().any(|e| e.0 == item) {
                kind.push("ENUM".into());
            } else if u.ranges.iter().any(|r| r.0 == item) {
                kind.push("RANGE".into());
            }
            if crate::fmt::parse_num(&u.vars[item]).is_some() {
                kind.push("NUMBER".into());
            } else {
                kind.push("STRING:32".into());
            }
            format!("TYPE {name} {item} {}", kind.join(" "))
        }
    }
}

/// A few descriptions in the style of NUT's cmdvartab.
fn var_desc(var: &str) -> &'static str {
    match var {
        "battery.charge" => "Battery charge (percent of full)",
        "battery.charge.low" => "Remaining battery level when UPS switches to LB (percent)",
        "battery.charge.warning" => "Battery level when UPS switches to Warning state (percent)",
        "battery.runtime" => "Battery runtime (seconds)",
        "battery.runtime.low" => "Remaining battery runtime when UPS switches to LB (seconds)",
        "battery.voltage" => "Battery voltage (V)",
        "battery.voltage.nominal" => "Nominal battery voltage (V)",
        "battery.temperature" => "Battery temperature (degrees C)",
        "battery.type" => "Battery chemistry",
        "input.voltage" => "Input voltage (V)",
        "input.frequency" => "Input line frequency (Hz)",
        "input.transfer.low" => "Low voltage transfer point (V)",
        "input.transfer.high" => "High voltage transfer point (V)",
        "input.sensitivity" => "Input power sensitivity",
        "output.voltage" => "Output voltage (V)",
        "output.current" => "Output current (A)",
        "ups.load" => "Load on UPS (percent of full)",
        "ups.realpower" => "Current value of real power (W)",
        "ups.power" => "Current value of apparent power (VA)",
        "ups.status" => "UPS status",
        "ups.beeper.status" => "UPS beeper status",
        "ups.delay.shutdown" => "Interval to wait after shutdown with delay command (seconds)",
        "ups.delay.start" => "Interval to wait before (re)starting the load (seconds)",
        "ups.test.result" => "Results of last self test",
        "ambient.temperature" => "Ambient temperature (degrees C)",
        "ambient.humidity" => "Ambient relative humidity (percent)",
        _ => "Description unavailable",
    }
}

fn find<'a>(sim: &'a mut Sim, name: &str) -> Result<&'a mut Unit, &'static str> {
    sim.units.iter_mut().find(|u| u.name == name).ok_or("UNKNOWN-UPS")
}

fn instcmd(sim: &mut Sim, name: &str, cmd: &str) -> Result<(), &'static str> {
    let u = find(sim, name)?;
    if !u.cmds.iter().any(|c| c.0 == cmd) {
        return Err("CMD-NOT-SUPPORTED");
    }
    let now = Instant::now();
    match cmd {
        "beeper.enable" => u.set("ups.beeper.status", "enabled"),
        "beeper.disable" => u.set("ups.beeper.status", "disabled"),
        "beeper.mute" => u.set("ups.beeper.status", "muted"),
        "test.battery.start.quick" | "test.battery.start.deep" => {
            let secs = if cmd.ends_with("deep") { 40 } else { 8 };
            u.test_until = Some(now + Duration::from_secs(secs));
            u.set("ups.test.result", "In progress");
        }
        "test.battery.stop" if u.test_until.take().is_some() => {
            u.set("ups.test.result", "Aborted");
        }
        "calibrate.start" => u.cal_until = Some(now + Duration::from_secs(30)),
        "calibrate.stop" => u.cal_until = None,
        "load.off" => u.output_off_until = Some(now + Duration::from_secs(3600)),
        "load.on" => u.output_off_until = None,
        "shutdown.return" => u.output_off_until = Some(now + Duration::from_secs(15)),
        _ => {}
    }
    Ok(())
}

fn set_var(sim: &mut Sim, name: &str, var: &str, value: &str) -> Result<(), &'static str> {
    let u = find(sim, name)?;
    if !u.vars.contains_key(var) {
        return Err("VAR-NOT-SUPPORTED");
    }
    if !u.rw.contains(&var) {
        return Err("READONLY");
    }
    if let Some((_, vals)) = u.enums.iter().find(|e| e.0 == var)
        && !vals.contains(&value)
    {
        return Err("INVALID-VALUE");
    }
    if let Some((_, (lo, hi))) = u.ranges.iter().find(|r| r.0 == var) {
        match value.trim().parse::<i64>() {
            Ok(v) if (*lo..=*hi).contains(&v) => {}
            _ => return Err("INVALID-VALUE"),
        }
    }
    u.set(var, value);
    Ok(())
}

/// One simulated UPS for render tests: (name, description, commands,
/// writable variables, one variable list per step).
#[cfg(test)]
pub type Recorded = (String, String, Vec<(String, String)>, Vec<String>, Vec<Vec<(String, String)>>);

/// Run the simulation for `steps` steps of `dt` seconds without a server.
#[cfg(test)]
pub fn record(steps: usize, dt: f64) -> Vec<Recorded> {
    let mut sim = Sim::new();
    let mut frames: Vec<Vec<Vec<(String, String)>>> = vec![Vec::new(); sim.units.len()];
    for _ in 0..steps {
        sim.step(dt);
        for (i, u) in sim.units.iter().enumerate() {
            frames[i].push(u.vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        }
    }
    sim.units
        .iter()
        .zip(frames)
        .map(|(u, f)| {
            let cmds = u.cmds.iter().map(|(c, d)| (c.to_string(), d.to_string())).collect();
            let rw = u.rw.iter().map(|s| s.to_string()).collect();
            (u.name.to_string(), u.desc.to_string(), cmds, rw, f)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nut::{Client, Tracking};

    fn client() -> Client {
        let s = start().unwrap();
        Client::connect(&s.host, s.port, Duration::from_secs(2)).unwrap()
    }

    #[test]
    fn lists_and_reads() {
        let mut c = client();
        let ups = c.list_ups().unwrap();
        assert_eq!(ups.iter().map(|u| u.0.as_str()).collect::<Vec<_>>(), ["rack", "pack"]);
        let vars = c.list_vars("rack").unwrap();
        assert!(vars.iter().any(|(k, _)| k == "ups.status"));
        assert_eq!(c.list_vars("nope").unwrap_err().code(), Some("UNKNOWN-UPS"));
        assert!(c.list_commands("rack").unwrap().contains(&"load.off".to_string()));
        assert_eq!(c.var_type("rack", "input.sensitivity").unwrap(), ["RW", "ENUM", "STRING:32"]);
        assert_eq!(c.list_enum("rack", "input.sensitivity").unwrap(), ["low", "medium", "high"]);
        assert_eq!(c.list_range("rack", "ups.delay.start").unwrap(), [("0".into(), "600".into())]);
        assert_eq!(c.var_description("rack", "nonsense").unwrap(), None);
        assert!(c.server_version().unwrap().contains("upsd"));
    }

    #[test]
    fn commands_need_login_and_track() {
        let mut c = client();
        assert_eq!(c.instcmd("rack", "beeper.mute", None).unwrap_err().code(), Some("USERNAME-REQUIRED"));
        c.authenticate("admin", "secret").unwrap();
        assert!(c.enable_tracking());
        let id = c.instcmd("rack", "beeper.mute", None).unwrap().unwrap();
        assert!(matches!(c.tracking(&id).unwrap(), Tracking::Pending));
        thread::sleep(Duration::from_millis(900));
        assert!(matches!(c.tracking(&id).unwrap(), Tracking::Success));
        let v = c.list_vars("rack").unwrap();
        assert!(v.contains(&("ups.beeper.status".into(), "muted".into())));
        assert_eq!(c.set_var("rack", "ups.delay.start", "9999").unwrap_err().code(), Some("INVALID-VALUE"));
        c.set_var("rack", "input.sensitivity", "high").unwrap();
        assert_eq!(c.set_var("rack", "ups.status", "OB").unwrap_err().code(), Some("READONLY"));
    }

    #[test]
    fn wrong_password_is_denied() {
        let mut c = client();
        c.authenticate("admin", "wrong").unwrap();
        let e = c.instcmd("rack", "beeper.mute", None).unwrap_err();
        assert!(e.is_auth());
    }
}
