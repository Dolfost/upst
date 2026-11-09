//! The config file. Every long option of upst and of `upst record` can go
//! in it as a key, plus settings per UPS. The command line and the
//! environment win over it.

use std::collections::BTreeMap;
use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use clap::ArgMatches;
use clap::parser::ValueSource;
use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

use crate::cli::{ChargeStyle, Cli, Command, ViewArg};
use crate::fmt::parse_duration;
use crate::recorder::Addr;
use crate::target::Target;

pub const SYSTEM: &str = "/etc/upst/config.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    targets: Option<Vec<Target>>,
    interval: Option<Time>,
    history: Option<Time>,
    timeout: Option<Time>,
    user: Option<String>,
    password_file: Option<PathBuf>,
    capacity: Option<f64>,
    view: Option<ViewArg>,
    charge_style: Option<ChargeStyle>,
    recorder: Option<Addr>,
    no_recorder: Option<bool>,
    ascii: Option<bool>,
    no_color: Option<bool>,
    no_mouse: Option<bool>,
    bell: Option<bool>,
    format: Option<String>,
    record: Record,
    ups: BTreeMap<String, UpsConfig>,
}

/// The `[record]` table, for `upst record`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct Record {
    targets: Option<Vec<Target>>,
    interval: Option<Time>,
    keep: Option<Time>,
    dir: Option<PathBuf>,
    listen: Option<Vec<Addr>>,
    timeout: Option<Time>,
}

/// A `[ups."name@host"]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct UpsConfig {
    capacity: Option<f64>,
}

/// A time as the options take it ("5s"), or a number of seconds.
#[derive(Clone, Copy, Debug)]
struct Time(Duration);

impl<'de> Deserialize<'de> for Time {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Time;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a time like \"5s\" or a number of seconds")
            }

            fn visit_str<E: de::Error>(self, s: &str) -> Result<Time, E> {
                parse_duration(s).map(Time).map_err(E::custom)
            }

            fn visit_i64<E: de::Error>(self, n: i64) -> Result<Time, E> {
                self.visit_f64(n as f64)
            }

            fn visit_u64<E: de::Error>(self, n: u64) -> Result<Time, E> {
                self.visit_f64(n as f64)
            }

            fn visit_f64<E: de::Error>(self, n: f64) -> Result<Time, E> {
                Duration::try_from_secs_f64(n).map(Time).map_err(|_| E::custom(format!("bad time {n}")))
            }
        }
        d.deserialize_any(V)
    }
}

/// Strings parsed the way the command line parses them.
macro_rules! from_str {
    ($($t:ty),*) => {$(
        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                String::deserialize(d)?.parse().map_err(de::Error::custom)
            }
        }
    )*};
}
from_str!(Target, Addr);

/// Where upst looks without --config: the system file, then the user's,
/// which wins key by key.
pub fn default_paths() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from(SYSTEM)];
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(dir) = dir {
        v.push(dir.join("upst/config.toml"));
    }
    v
}

/// The given file, or else the default ones that exist.
pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
    if let Some(path) = path {
        let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
        return parse(&text).with_context(|| path.display().to_string());
    }
    let mut cfg = Config::default();
    for path in default_paths() {
        match std::fs::read_to_string(&path) {
            Ok(text) => cfg = parse(&text).with_context(|| path.display().to_string())?.over(cfg),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| path.display().to_string()),
        }
    }
    Ok(cfg)
}

fn parse(text: &str) -> anyhow::Result<Config> {
    let cfg: Config = toml::from_str(text)?;
    for key in cfg.ups.keys() {
        let t: Target = key.parse().map_err(|e| anyhow::anyhow!("[ups.{key:?}]: {e}"))?;
        anyhow::ensure!(t.ups.is_some(), "[ups.{key:?}]: name one UPS, like \"ups@host\"");
    }
    Ok(cfg)
}

/// Whether the command line and the environment left this one alone.
fn free(m: &ArgMatches, id: &str) -> bool {
    matches!(m.value_source(id), None | Some(ValueSource::DefaultValue))
}

/// Fill in each field the config sets and the command line did not.
macro_rules! fill {
    ($dst:expr, $m:expr, $src:expr; $($f:ident => $conv:expr),* $(,)?) => {$(
        if let Some(v) = $src.$f {
            if free($m, stringify!($f)) {
                $dst.$f = $conv(v);
            }
        }
    )*};
}

/// Take each field `base` has and `self` leaves out.
macro_rules! under {
    ($dst:expr, $base:expr; $($f:ident),*) => {$(
        if $dst.$f.is_none() {
            $dst.$f = $base.$f;
        }
    )*};
}

impl Config {
    /// `self`, with what it leaves out taken from `base`.
    fn over(mut self, base: Config) -> Config {
        under!(self, base; targets, interval, history, timeout, user, password_file, capacity, view,
            charge_style, recorder, no_recorder, ascii, no_color, no_mouse, bell, format);
        under!(self.record, base.record; targets, interval, keep, dir, listen, timeout);
        for (key, b) in base.ups {
            let u = self.ups.entry(key).or_default();
            under!(u, b; capacity);
        }
        self
    }

    pub fn apply(self, cli: &mut Cli, m: &ArgMatches) {
        let time = |t: Time| t.0;
        if let (Some(Command::Record(args)), Some(m)) = (&mut cli.command, m.subcommand_matches("record")) {
            let r = self.record;
            let r = Record { targets: r.targets.or(self.targets), timeout: r.timeout.or(self.timeout), ..r };
            fill!(args, m, r;
                targets => std::convert::identity, interval => time, keep => time,
                dir => Some, listen => std::convert::identity, timeout => time);
            return;
        }
        // A capacity on the command line is for every UPS.
        if free(m, "capacity") {
            cli.capacities = self.ups.iter().filter_map(|(k, u)| Some((k.parse().ok()?, u.capacity?))).collect();
        }
        fill!(cli, m, self;
            targets => std::convert::identity, interval => time, history => time, timeout => time,
            user => Some, password_file => Some, capacity => Some, view => std::convert::identity,
            charge_style => std::convert::identity, recorder => Some, no_recorder => std::convert::identity,
            ascii => std::convert::identity, no_color => std::convert::identity,
            no_mouse => std::convert::identity, bell => std::convert::identity, format => Some);
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, FromArgMatches};

    use super::*;

    fn cli(cfg: &str, args: &[&str]) -> Cli {
        let m = Cli::command().try_get_matches_from(args).unwrap();
        let mut cli = Cli::from_arg_matches(&m).unwrap();
        parse(cfg).unwrap().apply(&mut cli, &m);
        cli
    }

    #[test]
    fn fills_what_the_command_line_leaves() {
        let cfg = r#"
            targets = ["ups@nas", "@pi"]
            interval = "10s"
            history = 7200
            charge-style = "lcd"
            capacity = 500
            bell = true
            no-recorder = true
            recorder = "nas"
            [ups."ups@nas"]
            capacity = 1024
        "#;
        let c = cli(cfg, &["upst"]);
        assert_eq!(c.targets.len(), 2);
        assert_eq!(c.interval, Duration::from_secs(10));
        assert_eq!(c.history, Duration::from_secs(7200));
        assert_eq!(c.charge_style, ChargeStyle::Lcd);
        assert_eq!(c.capacity, Some(500.0));
        assert!(c.bell && c.no_recorder);
        assert_eq!(c.recorder, Some("nas".parse().unwrap()));
        assert_eq!(c.capacities, vec![("ups@nas".parse().unwrap(), 1024.0)]);

        let c = cli(cfg, &["upst", "-i", "1s", "--charge-style", "bar", "--capacity", "100", "eco"]);
        assert_eq!(c.interval, Duration::from_secs(1));
        assert_eq!(c.charge_style, ChargeStyle::Bar);
        assert_eq!(c.targets, vec!["eco".parse().unwrap()]);
        assert_eq!(c.capacity, Some(100.0));
        assert!(c.capacities.is_empty());
        assert_eq!(c.history, Duration::from_secs(7200));
    }

    #[test]
    fn record_falls_back_to_the_top_level() {
        let cfg = r#"
            targets = ["ups@nas"]
            timeout = "9s"
            [record]
            keep = "30d"
            listen = ["/run/upst/upst.sock", "0.0.0.0:3494"]
        "#;
        let c = cli(cfg, &["upst", "record", "--keep", "2d"]);
        let Some(Command::Record(r)) = c.command else { panic!() };
        assert_eq!(r.targets, vec!["ups@nas".parse().unwrap()]);
        assert_eq!(r.timeout, Duration::from_secs(9));
        assert_eq!(r.keep, Duration::from_secs(2 * 86400));
        assert_eq!(r.listen.len(), 2);
        assert_eq!(r.interval, Duration::from_secs(5));
    }

    #[test]
    fn user_file_wins_key_by_key() {
        let system =
            parse("charge-style = \"lcd\"\nbell = true\n[ups.a]\ncapacity = 1\n[record]\nkeep = \"1d\"").unwrap();
        let user = parse("charge-style = \"bar\"\n[ups.b]\ncapacity = 2").unwrap();
        let c = user.over(system);
        assert_eq!(c.charge_style, Some(ChargeStyle::Bar));
        assert_eq!(c.bell, Some(true));
        assert_eq!(c.ups.len(), 2);
        assert!(c.record.keep.is_some());
    }

    #[test]
    fn mistakes_name_the_line() {
        for (text, want) in [
            ("colour = true", "unknown field `colour`"),
            ("interval = \"2x\"", "unknown unit"),
            ("charge-style = \"pie\"", "unknown variant `pie`"),
            ("targets = [\"a b\"]", "bad UPS name"),
            ("[record]\nkeep = -1", "bad time"),
            ("[ups.\"@nas\"]\ncapacity = 1", "name one UPS"),
        ] {
            let e = format!("{:#}", parse(text).unwrap_err());
            assert!(e.contains(want), "{text}: {e}");
        }
        assert!(format!("{:#}", parse("\n\nview = 3").unwrap_err()).contains("line 3"));
    }

    /// contrib/config.toml lists every key, commented out.
    #[test]
    fn example_file_has_every_key() {
        let text: String = include_str!("../contrib/config.toml")
            .lines()
            .filter_map(|l| l.strip_prefix("# "))
            .filter(|l| l.starts_with('[') || l.split_once(" = ").is_some_and(|(k, _)| !k.contains(' ')))
            .map(|l| format!("{l}\n"))
            .collect();
        let c = parse(&text).unwrap();
        macro_rules! present {
            ($c:expr; $($f:ident),*) => {$(
                assert!($c.$f.is_some(), concat!("contrib/config.toml lacks ", stringify!($f)));
            )*};
        }
        present!(c; targets, interval, history, timeout, user, password_file, capacity, view, charge_style,
            recorder, no_recorder, ascii, no_color, no_mouse, bell, format);
        present!(c.record; targets, interval, keep, dir, listen, timeout);
        assert!(c.ups.values().all(|u| u.capacity.is_some()) && !c.ups.is_empty());
    }
}
