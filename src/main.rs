mod app;
mod archive;
mod cli;
mod config;
mod demo;
mod fmt;
mod history;
mod journal;
mod model;
mod nut;
mod once;
mod recorder;
mod status;
mod target;
mod ui;
mod worker;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};

use app::{App, Options};
use cli::{Cli, Command, RecordArgs};
use recorder::{Addr, Recorder};
use target::{Server, Target};
use ui::Theme;
use worker::Update;

fn main() -> ExitCode {
    let matches = Cli::command().get_matches();
    let result = parse(&matches).and_then(|cli| match &cli.command {
        Some(Command::Record(args)) => record(args).map(|_| ExitCode::SUCCESS),
        None => run(cli),
    });
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("upst: {e:#}");
            ExitCode::from(1)
        }
    }
}

/// The command line, with the config file filling in what it leaves out.
fn parse(m: &ArgMatches) -> anyhow::Result<Cli> {
    // After another option, clap takes `record` for a UPS name.
    if m.value_source("targets") == Some(ValueSource::CommandLine)
        && m.get_raw("targets").and_then(|mut v| v.next()).is_some_and(|v| v == "record")
    {
        anyhow::bail!("record has to come first, as in: upst record --config FILE");
    }
    let mut cli = Cli::from_arg_matches(m)?;
    config::load(cli.config.as_deref())?.apply(&mut cli, m);
    Ok(cli)
}

/// Targets merged per server; `None` means every UPS on it.
fn group(targets: &[Target]) -> Vec<(Server, Option<Vec<String>>)> {
    let mut out: Vec<(Server, Option<Vec<String>>)> = Vec::new();
    for t in targets {
        match out.iter_mut().find(|(s, _)| *s == t.server) {
            None => out.push((t.server.clone(), t.ups.clone().map(|u| vec![u]))),
            Some((_, names)) => match (&t.ups, names.as_mut()) {
                (None, _) => *names = None,
                (Some(u), Some(v)) if !v.contains(u) => v.push(u.clone()),
                _ => {}
            },
        }
    }
    out
}

fn targets_or_localhost(targets: &[Target]) -> Vec<Target> {
    if targets.is_empty() { vec![Target { ups: None, server: Server::localhost() }] } else { targets.to_vec() }
}

fn password(cli: &Cli) -> anyhow::Result<Option<String>> {
    if let Some(path) = &cli.password_file {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        return Ok(Some(text.lines().next().unwrap_or("").to_string()));
    }
    Ok(std::env::var("UPST_PASSWORD").ok())
}

/// `upst record`.
fn record(args: &RecordArgs) -> anyhow::Result<()> {
    let dir = match &args.dir {
        Some(d) => d.clone(),
        None => recorder::default_dir().context("no --dir given and no state directory found")?,
    };
    let listen = if args.listen.is_empty() {
        vec![recorder::default_listen().context("no --listen given and no runtime directory found")?]
    } else {
        args.listen.clone()
    };
    let cfg = recorder::Config {
        groups: group(&targets_or_localhost(&args.targets)),
        interval: args.interval.max(Duration::from_millis(200)),
        timeout: args.timeout,
        keep: args.keep.max(Duration::from_secs(3600)),
        dir,
        listen,
        quiet: false,
    };
    Recorder::new(cfg)?.run()
}

/// The demo's upsd, and a recorder that already holds an hour of it.
fn start_demo(timeout: Duration) -> anyhow::Result<(Server, Addr, PathBuf)> {
    let (server, past) = demo::start_with_history(3600.0, 5.0).context("starting the demo server")?;
    let dir = std::env::temp_dir().join(format!("upst-demo-{}", std::process::id()));
    let sock = Addr::Unix(dir.join("upst.sock"));
    let cfg = recorder::Config {
        groups: vec![(server.clone(), None)],
        interval: Duration::from_secs(5),
        timeout,
        keep: Duration::from_secs(86400),
        dir: dir.clone(),
        listen: vec![sock.clone()],
        quiet: true,
    };
    let mut rec = Recorder::new(cfg).context("starting the demo recorder")?;
    for (name, frames) in past {
        rec.seed(0, &name, frames);
    }
    std::thread::spawn(move || rec.run());
    Ok((server, sock, dir))
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let mut demo_dir = None;
    let mut demo_recorder = None;
    let targets = if cli.demo {
        if cli.once || cli.json || cli.no_recorder {
            vec![Target { ups: None, server: demo::start().context("starting the demo server")? }]
        } else {
            let (server, sock, dir) = start_demo(cli.timeout)?;
            demo_dir = Some(dir);
            demo_recorder = Some(sock);
            vec![Target { ups: None, server }]
        }
    } else {
        targets_or_localhost(&cli.targets)
    };
    let groups = group(&targets);
    if cli.once || cli.json {
        return Ok(once::run(&groups, &cli));
    }

    let no_color_env = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let theme = Theme { color: !(cli.no_color || no_color_env), ascii: cli.ascii };
    let interval = cli.interval.max(Duration::from_millis(200));
    let mut app = App::new(Options {
        interval,
        history: cli.history.max(Duration::from_secs(60)),
        capacity: cli.capacity.filter(|c| *c > 0.0),
        capacities: cli.capacities.iter().filter(|(_, c)| *c > 0.0).cloned().collect(),
        bell: cli.bell,
        view: cli.view.into(),
        theme,
        charge_style: cli.charge_style,
        user: cli.user.clone(),
        password: password(&cli)?,
    });
    let (tx, rx) = mpsc::channel();
    // The recorder is asked first, so its answer is likely in before the
    // first polls.
    let recorder: Option<(Vec<Addr>, bool)> = match (demo_recorder, &cli.recorder) {
        (Some(sock), _) => Some((vec![sock], true)),
        (None, _) if cli.no_recorder => None,
        (None, Some(addr)) => Some((vec![addr.clone()], true)),
        (None, None) => {
            let found: Vec<Addr> =
                recorder::default_sockets().into_iter().filter(|p| p.exists()).map(Addr::Unix).collect();
            (!found.is_empty()).then_some((found, false))
        }
    };
    if let Some((addrs, named)) = recorder {
        app.set_recorder(archive::spawn(addrs, named, cli.timeout, tx.clone()));
    }
    for (index, (server, names)) in groups.iter().enumerate() {
        let cfg = worker::Config {
            index,
            server: server.clone(),
            names: names.clone(),
            interval,
            timeout: cli.timeout,
            meta: true,
        };
        let req = worker::spawn(cfg, tx.clone());
        app.add_server(server.clone(), req, names.as_deref());
    }
    drop(tx);
    let result = tui(&mut app, rx, !cli.no_mouse);
    if let Some(dir) = demo_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    result?;
    Ok(ExitCode::SUCCESS)
}

fn restore(mouse: bool) {
    let _ = disable_raw_mode();
    let mut out = io::stdout();
    if mouse {
        let _ = execute!(out, DisableMouseCapture);
    }
    let _ = execute!(out, LeaveAlternateScreen, ratatui::crossterm::cursor::Show);
}

fn tui(app: &mut App, rx: Receiver<Update>, mouse: bool) -> anyhow::Result<()> {
    enable_raw_mode().context("enabling raw mode (is stdout a terminal?)")?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore(mouse);
        hook(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    let result = event_loop(&mut terminal, app, rx);
    restore(mouse);
    result
}

/// Base64 for OSC 52, which is all the clipboard support needs.
fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn event_loop<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    app: &mut App,
    rx: Receiver<Update>,
) -> anyhow::Result<()> {
    let mut dirty = true;
    let mut last_draw = Instant::now();
    loop {
        while let Ok(u) = rx.try_recv() {
            app.update(u);
            dirty = true;
        }
        if dirty || last_draw.elapsed() >= Duration::from_secs(1) {
            app.tick();
            terminal.draw(|f| ui::draw(f, app))?;
            last_draw = Instant::now();
            dirty = false;
            let backend = terminal.backend_mut();
            if std::mem::take(&mut app.bell_pending) {
                backend.write_all(b"\x07")?;
            }
            if let Some(text) = app.clipboard.take() {
                write!(backend, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
            }
            backend.flush()?;
        }
        if event::poll(Duration::from_millis(100))? {
            loop {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k),
                    Event::Mouse(m) => app.on_mouse(m),
                    _ => {}
                }
                dirty = true;
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if app.quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_targets_by_server() {
        let t: Vec<Target> = ["a@x", "b@x", "@y", "c@y", "a@x"].iter().map(|s| s.parse().unwrap()).collect();
        let g = group(&t);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].1.as_deref(), Some(&["a".to_string(), "b".to_string()][..]));
        assert_eq!(g[1].1, None);
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
