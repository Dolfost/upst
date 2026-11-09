//! Recorded history for the dashboard: a thread that asks `upst record`
//! for the journals of the UPSes on screen, and the replay that turns a
//! journal into charts, events and estimates as if upst had been running
//! all along.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use crate::journal::{GAP, Kind, Record};
use crate::model::{Link, Ups, from_unix};
use crate::nut::DEFAULT_PORT;
use crate::recorder::{Addr, Client, Listed};
use crate::status::Severity;
use crate::target::{Server, parse_server};
use crate::worker::Update;

pub struct Fetch {
    pub server: usize,
    pub ups: String,
    pub label: String,
    pub since: f64,
}

#[derive(Debug)]
pub enum Event {
    Ready {
        addr: Addr,
        list: Vec<Listed>,
    },
    /// No recorder; the error is set when one was asked for by name.
    Off {
        error: Option<String>,
    },
    History {
        server: usize,
        ups: String,
        label: String,
        result: Result<Vec<Record>, String>,
    },
}

/// Connect to the first recorder that answers and serve fetches from it.
pub fn spawn(addrs: Vec<Addr>, named: bool, timeout: Duration, tx: Sender<Update>) -> Sender<Fetch> {
    let (req, rx) = mpsc::channel::<Fetch>();
    thread::Builder::new()
        .name("upst recorder".into())
        .spawn(move || {
            let mut error = None;
            let mut found = None;
            for addr in addrs {
                match Client::connect(&addr, timeout).and_then(|mut c| {
                    let v = c.version()?;
                    if !v.starts_with("upst ") {
                        return Err(std::io::Error::other(format!("not an upst recorder (it says {v:?})")));
                    }
                    c.list().map(|l| (c, l))
                }) {
                    Ok((c, list)) => {
                        found = Some((addr, c, list));
                        break;
                    }
                    Err(e) => error = Some(format!("{addr}: {e}")),
                }
            }
            let Some((addr, client, list)) = found else {
                let _ = tx.send(Update::Archive(Event::Off { error: error.filter(|_| named) }));
                return;
            };
            if tx.send(Update::Archive(Event::Ready { addr: addr.clone(), list })).is_err() {
                return;
            }
            let mut client = Some(client);
            for f in rx {
                let get = |c: &mut Option<Client>| -> std::io::Result<Vec<Record>> {
                    if c.is_none() {
                        *c = Some(Client::connect(&addr, timeout)?);
                    }
                    let r = c.as_mut().unwrap().get(&f.label, f.since);
                    if r.is_err() {
                        *c = None;
                    }
                    r
                };
                // An idle connection may have been dropped: one retry.
                let result = get(&mut client).or_else(|_| get(&mut client)).map_err(|e| e.to_string());
                let ev = Event::History { server: f.server, ups: f.ups, label: f.label, result };
                if tx.send(Update::Archive(ev)).is_err() {
                    return;
                }
            }
        })
        .expect("spawn recorder thread");
    req
}

fn loopback(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost") || h == "::1" || h.starts_with("127.")
}

/// The recorder's journal for a UPS: the same label, counting the
/// recorder's own `localhost` as the host it runs on, or else the only
/// journal with that UPS name.
pub fn matching<'a>(list: &'a [Listed], name: &str, server: &Server, via: &Addr) -> Option<&'a str> {
    let same = |label: &str| {
        let Some((n, host)) = label.split_once('@') else { return false };
        let Ok(mut s) = parse_server(host, DEFAULT_PORT) else { return false };
        if let Addr::Tcp(r) = via
            && loopback(&s.host)
        {
            s.host = r.host.clone();
        }
        let host_eq = s.host.eq_ignore_ascii_case(&server.host) || (loopback(&s.host) && loopback(&server.host));
        n == name && s.port == server.port && host_eq
    };
    if let Some(l) = list.iter().find(|l| same(&l.label)) {
        return Some(&l.label);
    }
    let mut named = list.iter().filter(|l| l.label.split_once('@').is_some_and(|(n, _)| n == name));
    match (named.next(), named.next()) {
        (Some(l), None) => Some(&l.label),
        _ => None,
    }
}

/// Feed a journal into a UPS as if its polls had happened live, up to
/// `until`. Returns the events it implied, with their times.
pub fn replay(u: &mut Ups, records: &[Record], until: f64) -> Vec<(f64, Severity, String)> {
    let mut events = Vec::new();
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    // Polls were succeeding at the last record.
    let mut up = false;
    // A poll to apply once its records are all read.
    let mut pending: Option<f64> = None;
    let mut last: Option<f64> = None;
    // upsd was lost, as opposed to the recorder not running.
    let mut lost = false;
    let apply = |u: &mut Ups, vars: &BTreeMap<String, String>, t: f64, events: &mut Vec<(f64, Severity, String)>| {
        let list = vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        events.extend(u.apply(list, from_unix(t)).into_iter().map(|(s, m)| (t, s, m)));
    };
    for r in records.iter().take_while(|r| r.t < until) {
        if let Some(p) = pending.filter(|p| *p != r.t) {
            apply(u, &vars, p, &mut events);
            pending = None;
        }
        if let Some(l) = last.filter(|l| up && r.t - l > GAP) {
            u.gap(from_unix(l));
            up = false;
        }
        match &r.kind {
            Kind::Start => {
                if let Some(l) = last.filter(|_| up) {
                    u.gap(from_unix(l));
                }
                up = false;
            }
            Kind::Up => {
                // As live upst says when a lost connection comes back.
                if std::mem::take(&mut lost) {
                    events.push((r.t, Severity::Info, format!("Reconnected to {}", u.server_label)));
                }
                up = true;
                pending = Some(r.t);
            }
            Kind::Down(why) | Kind::NoData(why) => {
                up = false;
                lost |= matches!(r.kind, Kind::Down(_));
                let link =
                    if matches!(r.kind, Kind::Down(_)) { Link::Down(why.clone()) } else { Link::Stale(why.clone()) };
                if let Some((sev, text)) = u.lost_at(link, from_unix(r.t)) {
                    events.push((r.t, sev, text));
                }
            }
            Kind::Tick => {
                if up {
                    pending = Some(r.t);
                }
            }
            Kind::Set(k, v) => {
                vars.insert(k.clone(), v.clone());
                if up {
                    pending = Some(r.t);
                }
            }
            Kind::Unset(k) => {
                vars.remove(k);
                if up {
                    pending = Some(r.t);
                }
            }
        }
        last = Some(r.t);
    }
    if let Some(p) = pending {
        apply(u, &vars, p, &mut events);
    }
    if let Some(l) = last.filter(|l| up && until - l > GAP) {
        u.gap(from_unix(l));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{Band, History};
    use crate::model::unix;

    fn listed(labels: &[&str]) -> Vec<Listed> {
        labels.iter().map(|l| Listed { label: l.to_string(), first: 0.0, last: 0.0 }).collect()
    }

    fn srv(host: &str, port: u16) -> Server {
        Server { host: host.into(), port }
    }

    #[test]
    fn matches_labels() {
        let unix_sock = Addr::Unix("/run/upst/upst.sock".into());
        let l = listed(&["ups@localhost", "rack@nas:3500", "rack@pi"]);
        assert_eq!(matching(&l, "ups", &srv("127.0.0.1", 3493), &unix_sock), Some("ups@localhost"));
        assert_eq!(matching(&l, "rack", &srv("nas", 3500), &unix_sock), Some("rack@nas:3500"));
        assert_eq!(matching(&l, "rack", &srv("pi", 3493), &unix_sock), Some("rack@pi"));
        // Two journals named rack, neither on this host.
        assert_eq!(matching(&l, "rack", &srv("other", 3493), &unix_sock), None);
        // A remote recorder's localhost is the host it runs on.
        let remote = Addr::Tcp(srv("nas", 3494));
        let l = listed(&["ups@localhost", "ups@elsewhere"]);
        assert_eq!(matching(&l, "ups", &srv("nas", 3493), &remote), Some("ups@localhost"));
        // Only one journal for that name: take it.
        assert_eq!(
            matching(&listed(&["ups@localhost"]), "ups", &srv("nas.lan", 3493), &unix_sock),
            Some("ups@localhost")
        );
    }

    fn rec(t: f64, kind: Kind) -> Record {
        Record::new(t, kind)
    }

    fn set(t: f64, k: &str, v: &str) -> Record {
        rec(t, Kind::Set(k.into(), v.into()))
    }

    #[test]
    fn replays_outages_and_gaps() {
        let t0 = unix(std::time::SystemTime::now()).floor() - 3000.0;
        let records = vec![
            rec(t0, Kind::Start),
            rec(t0, Kind::Up),
            set(t0, "ups.status", "OL"),
            set(t0, "battery.charge", "100"),
            set(t0 + 60.0, "ups.status", "OB DISCHRG"),
            set(t0 + 120.0, "battery.charge", "99"),
            set(t0 + 180.0, "battery.charge", "98"),
            set(t0 + 240.0, "ups.status", "OL CHRG"),
            rec(t0 + 300.0, Kind::Tick),
            rec(t0 + 330.0, Kind::Down("Connection refused".into())),
            rec(t0 + 400.0, Kind::Up),
            // The recorder stopped here; it starts again 1000 s later.
            rec(t0 + 420.0, Kind::Tick),
            rec(t0 + 1420.0, Kind::Start),
            rec(t0 + 1420.0, Kind::Up),
            set(t0 + 1420.0, "battery.charge", "100"),
            set(t0 + 1420.0, "ups.status", "OL"),
        ];
        let mut u = Ups::new(0, "localhost", "ups", "", History::new(86400.0, 2.0), None);
        let events = replay(&mut u, &records, t0 + 1500.0);
        let texts: Vec<&str> = events.iter().map(|e| e.2.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Power failure, running on battery",
                "Power restored after 3m 00s on battery",
                "Lost upsd: Connection refused",
                "Reconnected to localhost"
            ]
        );
        assert_eq!(events[0].0, t0 + 60.0);
        assert_eq!(u.outages, 1);
        assert_eq!(u.since, Some(from_unix(t0)));
        assert_eq!(u.get("battery.charge"), Some("100"));
        assert!(u.is_live());
        let bands = u.history.band_columns(t0, t0 + 1500.0, 15, t0 + 1500.0);
        assert_eq!(bands[0], Band::Online);
        assert_eq!(bands[1], Band::Battery);
        assert_eq!(bands[3], Band::NoData);
        assert_eq!(bands[5], Band::NoData);
        assert_eq!(bands[14], Band::Online);
        let charge = u.history.get("battery.charge").unwrap();
        assert_eq!(charge.at(t0 + 200.0), Some(98.0));

        // Records from `until` on are left to the live polls.
        let mut u = Ups::new(0, "localhost", "ups", "", History::new(86400.0, 2.0), None);
        replay(&mut u, &records, t0 + 100.0);
        assert_eq!(u.status.power(), crate::status::Power::Battery);
    }
}
