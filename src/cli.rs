use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Deserialize;

use crate::fmt::parse_duration;
use crate::recorder::Addr;
use crate::target::Target;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewArg {
    Overview,
    History,
    Variables,
    Events,
}

/// How the Battery card draws the charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChargeStyle {
    /// A battery gauge with the percentage inside
    Battery,
    /// Seven-segment digits like a UPS front panel, over a bar
    Lcd,
    /// A row of LED segments colored by the low and warning levels
    Segments,
    /// Big pixel digits over a bar
    Blocks,
    /// Only the bar, the most compact
    Bar,
}

impl ChargeStyle {
    pub fn name(self) -> &'static str {
        match self {
            ChargeStyle::Battery => "battery",
            ChargeStyle::Lcd => "lcd",
            ChargeStyle::Segments => "segments",
            ChargeStyle::Blocks => "blocks",
            ChargeStyle::Bar => "bar",
        }
    }

    pub fn next(self) -> Self {
        let all = ChargeStyle::value_variants();
        let i = all.iter().position(|s| *s == self).unwrap_or(0);
        all[(i + 1) % all.len()]
    }
}

/// Terminal dashboard for UPSes served by Network UPS Tools (upsd).
#[derive(Parser, Debug)]
#[command(version, about, long_about = None, after_help = AFTER_HELP, args_conflicts_with_subcommands = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// UPS to watch: ups[@host[:port]], or @host[:port] for every UPS on
    /// that server. Repeatable. Default: every UPS on localhost.
    #[arg(value_name = "TARGET", env = "UPST_TARGETS", value_delimiter = ',')]
    pub targets: Vec<Target>,

    /// Time between polls (e.g. 2, 500ms, 10s)
    #[arg(short, long, default_value = "1s", value_parser = parse_duration, value_name = "TIME")]
    pub interval: Duration,

    /// How much history to keep for charts and trends, and to load from
    /// the recorder
    #[arg(long, default_value = "24h", value_parser = parse_duration, value_name = "TIME")]
    pub history: Duration,

    /// Network timeout for connects and replies
    #[arg(long, default_value = "5s", value_parser = parse_duration, value_name = "TIME")]
    pub timeout: Duration,

    /// upsd user for instant commands and writable variables
    #[arg(short, long, env = "UPST_USER")]
    pub user: Option<String>,

    /// File whose first line is the upsd password (or set UPST_PASSWORD)
    #[arg(long, env = "UPST_PASSWORD_FILE", value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// Battery capacity in Wh, for energy and power-draw estimates
    #[arg(long, value_name = "WH")]
    pub capacity: Option<f64>,

    /// View to start on
    #[arg(long, value_enum, default_value_t = ViewArg::Overview)]
    pub view: ViewArg,

    /// How to draw the battery charge; b switches while running
    #[arg(long, value_enum, env = "UPST_CHARGE_STYLE", default_value_t = ChargeStyle::Battery, value_name = "STYLE")]
    pub charge_style: ChargeStyle,

    /// Load recorded history from this `upst record`: a socket path or
    /// host[:port]. Default: /run/upst/upst.sock or
    /// $XDG_RUNTIME_DIR/upst/upst.sock when one exists
    #[arg(long, env = "UPST_RECORDER", value_name = "ADDR")]
    pub recorder: Option<Addr>,

    /// Do not load recorded history, even with --recorder or UPST_RECORDER
    #[arg(long)]
    pub no_recorder: bool,

    /// Plain ASCII bars, sparklines and charts, for the Linux console and
    /// fonts without braille or block glyphs
    #[arg(long)]
    pub ascii: bool,

    /// No colors (NO_COLOR is honored too)
    #[arg(long)]
    pub no_color: bool,

    /// Leave the mouse to the terminal
    #[arg(long)]
    pub no_mouse: bool,

    /// Ring the terminal bell on power events
    #[arg(long)]
    pub bell: bool,

    /// Print one line per UPS and exit
    #[arg(long)]
    pub once: bool,

    /// Line template for --once, e.g. "{battery.charge}% {status}"
    #[arg(long, requires = "once", value_name = "TEMPLATE")]
    pub format: Option<String>,

    /// Print every variable of every UPS as JSON and exit
    #[arg(long, conflicts_with = "once")]
    pub json: bool,

    /// Watch a simulated upsd with two UPSes and an hour of recorded
    /// history instead of a real one
    #[arg(long)]
    pub demo: bool,

    /// Config file to read instead of /etc/upst/config.toml and
    /// ~/.config/upst/config.toml
    #[arg(long, global = true, env = "UPST_CONFIG", value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Capacities per UPS, from the config file.
    #[arg(skip)]
    pub capacities: Vec<(Target, f64)>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Record UPS history in the background and serve it to upst
    #[command(after_help = RECORD_HELP)]
    Record(RecordArgs),
}

#[derive(Args, Debug)]
pub struct RecordArgs {
    /// UPS to record, as for upst itself. Default: every UPS on localhost.
    #[arg(value_name = "TARGET", value_delimiter = ',')]
    pub targets: Vec<Target>,

    /// Time between polls
    #[arg(short, long, default_value = "5s", value_parser = parse_duration, value_name = "TIME")]
    pub interval: Duration,

    /// How long to keep history
    #[arg(long, default_value = "7d", value_parser = parse_duration, value_name = "TIME")]
    pub keep: Duration,

    /// Directory for the journals. Default: $STATE_DIRECTORY (set by
    /// systemd), else $XDG_STATE_HOME/upst or ~/.local/state/upst
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// Where to serve history: a socket path or host:port, repeatable.
    /// Default: $RUNTIME_DIRECTORY/upst.sock (set by systemd), else
    /// $XDG_RUNTIME_DIR/upst/upst.sock
    #[arg(long, value_name = "ADDR")]
    pub listen: Vec<Addr>,

    /// Network timeout for connects and replies
    #[arg(long, default_value = "5s", value_parser = parse_duration, value_name = "TIME")]
    pub timeout: Duration,
}

const AFTER_HELP: &str = "\
Examples:
  upst                      every UPS on localhost
  upst ups@nas.lan          one UPS on another host
  upst @nas.lan rack@pi     everything on nas.lan plus one UPS on pi
  upst --demo               try it without a UPS
  upst --once --format '{battery.charge}% {runtime}'
  upst record               keep history for upst to show (see upst record --help)

--format placeholders: any variable name ({battery.charge}), plus {ups},
{status} (e.g. \"ON BATTERY\"), {flags} (raw ups.status), {charge},
{runtime} (formatted) and {load}.

Every long option can also be set in /etc/upst/config.toml or
~/.config/upst/config.toml; the command line and UPST_* variables win.

Press ? inside upst for the keys.";

const RECORD_HELP: &str = "\
upst record polls like upst does, writes every change to a journal per UPS
and serves the journals, so upst opens with the history of the last days.
It is meant to run as a service; the README has a systemd unit and a NixOS
module. Its options can also go in the [record] table of the config file,
where targets and timeout default to upst's own. upst finds a recorder on /run/upst/upst.sock or
$XDG_RUNTIME_DIR/upst/upst.sock by itself; elsewhere pass --recorder.

Examples:
  upst record                                    every UPS on localhost
  upst record ups@localhost --keep 30d
  upst record --listen /run/upst/upst.sock --listen 0.0.0.0:3494";
