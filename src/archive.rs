//! Recorded history for the dashboard: a thread that asks `upst record`
//! for the journals of the UPSes on screen, for `Ups::replay` to turn into
//! charts, events and estimates as if upst had been running all along.

use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use upst_core::recorder::{Addr, Client, Listed};

use crate::app::Update;
use crate::journal::Record;
use crate::nut::DEFAULT_PORT;
use crate::target::{Server, parse_server};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
