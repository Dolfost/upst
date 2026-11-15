//! The core of [upst]: what it knows about UPSes monitored by
//! [Network UPS Tools], without the terminal UI.
//!
//! - [`nut`]: a small blocking client for upsd, and [`target`] for the
//!   `ups@host:port` syntax.
//! - [`Ups`]: the state of one UPS, fed with polls. It turns them into
//!   typed [`Event`]s with stable codes (power lost, power restored after
//!   2 hours, battery low...), outage statistics, and an [`Estimate`] of the
//!   time to low battery or to full.
//! - [`poll`]: a background thread per upsd server that polls, reconnects
//!   and runs commands.
//! - [`journal`] and `recorder`: the history that `upst record` keeps,
//!   and a client to fetch it (on Unix), to [replay](Ups::replay) into a
//!   [`Ups`].
//! - `demo` (feature `demo`): a simulated upsd, for tests.
//!
//! The crate uses `std` only. It never prints and never touches the disk
//! unless asked to (the [`journal`] writer). The state of a UPS never reads
//! the clock: every change comes with the time it happened, so polls,
//! recorded journals and tests go through the same code. Only the
//! [`poll`]er and the demo server, which run threads, look at the clock;
//! the poller stamps each poll with the time its answer came.
//!
//! # Example
//!
//! Connect, poll, and turn the polls into events and estimates:
//!
//! ```no_run
//! use std::time::{Duration, SystemTime};
//! use upst_core::{Ups, nut};
//!
//! let mut client = nut::Client::connect("localhost", nut::DEFAULT_PORT, Duration::from_secs(5))?;
//! let mut ups = Ups::builder("eco", "localhost").history(None).build();
//! loop {
//!     let vars = client.list_vars("eco")?;
//!     let now = SystemTime::now();
//!     for ev in ups.apply(vars, now) {
//!         // `code` for rules, `severity` for how loud, `kind` for the details.
//!         println!("{} {:?}: {ev}", ev.code(), ev.severity);
//!     }
//!     if let Some(est) = ups.estimate(now) {
//!         println!("to low battery: {:?} s, drawing {:?} W", est.to_low, est.watts);
//!     }
//!     std::thread::sleep(Duration::from_secs(2));
//! }
//! # Ok::<(), nut::Error>(())
//! ```
//!
//! The same against the `demo` server, which this crate's own tests
//! use (needs the `demo` feature):
//!
//! ```
//! # #[cfg(feature = "demo")]
//! # {
//! use std::time::{Duration, SystemTime};
//! use upst_core::{Ups, demo, nut};
//!
//! let server = demo::Server::spawn()?;
//! let mut client = nut::Client::connect("127.0.0.1", server.addr().port(), Duration::from_secs(2))?;
//! let mut ups = Ups::builder("rack", "demo").history(None).build();
//! let events = ups.apply(client.list_vars("rack")?, SystemTime::now());
//! assert!(ups.is_live() && ups.num("battery.charge").is_some());
//! assert!(events.iter().all(|e| e.code() == "power.started_on_battery"));
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Features
//!
//! - `serde`: `Serialize` and `Deserialize` for [`Event`], [`EventKind`],
//!   [`Severity`], [`Power`], [`Status`], [`Link`], [`Estimate`], [`Var`]
//!   and the like.
//! - `demo`: the `demo` module, a simulated upsd with two UPSes.
//!
//! With `serde`, an event looks like this in JSON (times as serde has
//! them for [`SystemTime`](std::time::SystemTime) and
//! [`Duration`](std::time::Duration)):
//!
//! ```json
//! {
//!   "at": { "secs_since_epoch": 1700000130, "nanos_since_epoch": 0 },
//!   "severity": "good",
//!   "kind": { "type": "power_restored", "on_battery": { "secs": 120, "nanos": 0 } }
//! }
//! ```
//!
//! The `type` tag is the variant name in snake case; it is not the event
//! code, so keep [`Event::code`] next to it if rules need the code later.
//!
//! # Stability
//!
//! Event codes ([`Event::code`]) and status flag codes
//! ([`status::flag_info`]) never change meaning. The [`journal`] format and
//! the recorder protocol only grow. Until 1.0, other breaking changes come
//! with a new minor version and are listed in the changelog; that includes
//! the serialized forms.
//!
//! [upst]: https://github.com/Dolfost/upst
//! [Network UPS Tools]: https://networkupstools.org/

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "demo")]
pub mod demo;
pub mod event;
pub mod fmt;
pub mod history;
pub mod journal;
pub mod model;
pub mod nut;
pub mod poll;
#[cfg(unix)]
pub mod recorder;
pub mod status;
pub mod target;

pub use event::{Event, EventKind};
pub use model::{Estimate, Link, Ups, UpsBuilder, Var, from_unix, unix};
pub use status::{Power, Severity, Status};

#[cfg(test)]
mod tests {
    use super::*;

    fn send<T: Send>() {}
    fn sync<T: Sync>() {}

    #[test]
    fn types_cross_threads() {
        send::<Ups>();
        send::<Event>();
        send::<Estimate>();
        send::<nut::Client>();
        send::<poll::Poller>();
        send::<poll::Update>();
        #[cfg(unix)]
        send::<recorder::Client>();
        sync::<Ups>();
        sync::<Event>();
    }
}
