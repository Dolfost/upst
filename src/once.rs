//! Non-interactive output: `--once` (a line per UPS, optionally from a
//! template, handy for status bars) and `--json`.

use std::process::ExitCode;

use crate::cli::Cli;
use crate::fmt;
use crate::nut::Client;
use crate::status::Status;
use crate::target::Server;

pub fn run(groups: &[(Server, Option<Vec<String>>)], cli: &Cli) -> ExitCode {
    let mut failed = false;
    let mut json = Vec::new();
    for (server, names) in groups {
        let mut c = match Client::connect(&server.host, server.port, cli.timeout) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("upst: {server}: {e}");
                failed = true;
                continue;
            }
        };
        let names = match names {
            Some(n) => n.clone(),
            None => match c.list_ups() {
                Ok(l) => l.into_iter().map(|(n, _)| n).collect(),
                Err(e) => {
                    eprintln!("upst: {server}: {e}");
                    failed = true;
                    continue;
                }
            },
        };
        for name in names {
            let label = format!("{name}@{server}");
            match c.list_vars(&name) {
                Ok(vars) if cli.json => json.push((label, vars)),
                Ok(vars) => println!("{}", line(&label, &vars, cli.format.as_deref())),
                Err(e) => {
                    eprintln!("upst: {label}: {e}");
                    failed = true;
                }
            }
        }
        c.logout();
    }
    if cli.json {
        println!("{}", to_json(&json));
    }
    if failed { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

pub fn line(label: &str, vars: &[(String, String)], template: Option<&str>) -> String {
    let get = |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.trim()).filter(|v| !v.is_empty());
    let status = Status::parse(get("ups.status").unwrap_or(""));
    let lookup = |key: &str| -> Option<String> {
        match key {
            "ups" => Some(label.to_string()),
            "status" => Some(status.headline().0.to_string()),
            "flags" => get("ups.status").map(str::to_string),
            "charge" => get("battery.charge").map(|c| format!("{c}%")),
            "runtime" => get("battery.runtime").and_then(fmt::parse_num).map(fmt::duration),
            "load" => get("ups.load").map(|l| format!("{l}%")),
            k => get(k).map(str::to_string),
        }
    };
    if let Some(t) = template {
        return render(t, lookup);
    }
    let mut parts = vec![lookup("status").unwrap_or_default()];
    if let Some(c) = lookup("charge") {
        parts.push(format!("battery {c}"));
    }
    if let Some(r) = lookup("runtime") {
        parts.push(format!("runtime {r}"));
    }
    if let Some(l) = lookup("load") {
        parts.push(format!("load {l}"));
    }
    format!("{label}: {}", parts.join(", "))
}

/// `{name}` becomes its value or `-`; `{{` and `}}` are literal braces.
fn render(t: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let key: String = chars.by_ref().take_while(|c| *c != '}').collect();
                out.push_str(&lookup(key.trim()).unwrap_or_else(|| "-".into()));
            }
            c => out.push(c),
        }
    }
    out
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn to_json(all: &[(String, Vec<(String, String)>)]) -> String {
    let objs: Vec<String> = all
        .iter()
        .map(|(label, vars)| {
            let fields: Vec<String> = vars.iter().map(|(k, v)| format!("{}:{}", json_str(k), json_str(v))).collect();
            format!("{}:{{{}}}", json_str(label), fields.join(","))
        })
        .collect();
    format!("{{{}}}", objs.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vec<(String, String)> {
        [("ups.status", "OB DISCHRG"), ("battery.charge", "76"), ("battery.runtime", "3725")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn default_line() {
        assert_eq!(line("ups@localhost", &vars(), None), "ups@localhost: ON BATTERY, battery 76%, runtime 1h 02m");
    }

    #[test]
    fn templates() {
        let l = line("u", &vars(), Some("{charge} {battery.charge} {flags} {nope} {{x}}"));
        assert_eq!(l, "76% 76 OB DISCHRG - {x}");
    }

    #[test]
    fn json_escapes() {
        let j = to_json(&[("a@b".into(), vec![("k".into(), "say \"hi\"\n".into())])]);
        assert_eq!(j, r#"{"a@b":{"k":"say \"hi\"\n"}}"#);
    }
}
