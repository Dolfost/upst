# upst-core

The core of [upst](https://github.com/Dolfost/upst), the terminal dashboard for UPSes monitored by
[Network UPS Tools](https://networkupstools.org/), as a library without the terminal UI:

- a small blocking client for upsd (`nut`), and the `ups@host:port` syntax (`target`);
- `Ups`, the state of one UPS fed with polls, which turns them into typed events with stable codes
  (power lost, power restored after 2 hours, battery low...), outage statistics, and estimates of the
  time to low battery or to full;
- a poller thread per upsd server that reconnects, takes a new interval on the fly, and runs commands
  (`poll`);
- the journal that `upst record` keeps, a client for the recorder (on Unix), and `Ups::replay` to
  rebuild the state and events from it (`journal`, `recorder`);
- a simulated upsd with two UPSes, for tests (`demo`, behind a feature).

It uses `std` only, needs no async runtime (run the client or the poller on a thread of its own),
and never prints. The state of a UPS never reads the clock: every change comes with the time it
happened, so live polls, recorded journals and tests go through the same code.

```rust,no_run
use std::time::{Duration, SystemTime};
use upst_core::{Ups, nut};

fn main() -> Result<(), nut::Error> {
    let mut client = nut::Client::connect("localhost", nut::DEFAULT_PORT, Duration::from_secs(5))?;
    let mut ups = Ups::builder("eco", "localhost").history(None).build();
    loop {
        let vars = client.list_vars("eco")?;
        let now = SystemTime::now();
        for ev in ups.apply(vars, now) {
            // `code` for rules, `severity` for how loud, `kind` for the details.
            println!("{} {:?}: {ev}", ev.code(), ev.severity);
        }
        if let Some(est) = ups.estimate(now) {
            println!("to low battery: {:?} s, drawing {:?} W", est.to_low, est.watts);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
```

## Features

- `serde`: `Serialize` and `Deserialize` for events, estimates, status, links and variables.
- `demo`: `demo::Server`, a simulated upsd on a loopback port, and `demo::simulate` for its polls
  without a server.

## Stability

Event codes (`Event::code`, e.g. `power.lost`, `power.restored`, `battery.low`) and status flag codes
(`status::flag_info`) never change meaning; new kinds of events get new codes. The journal format
(`upst journal 1`) and the recorder protocol (version 1) only grow. Until 1.0, other breaking changes
come with a new minor version and are listed in [CHANGELOG.md](CHANGELOG.md).

The minimum supported Rust version is 1.88.

## License

Apache-2.0
