//! Static checks over the crate's own sources: it never prints, its
//! state logic never reads the clock, it depends on serde at most, and it
//! has no unsafe code.

use std::fs;
use std::path::{Path, PathBuf};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Every `.rs` file under `src`, with its path relative to `src`.
fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let src = root().join("src");
    let mut paths = Vec::new();
    walk(&src, &mut paths);
    paths.sort();
    assert!(paths.len() >= 10, "found only {paths:?}");
    paths
        .into_iter()
        .map(|p| (p.strip_prefix(&src).unwrap().display().to_string(), fs::read_to_string(&p).unwrap()))
        .collect()
}

fn ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The source with comments (doc comments too) dropped and the insides of
/// string and char literals blanked; newlines stay, so lines still match.
fn code_only(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let blank = |out: &mut String, ch: char| out.push(if ch == '\n' { '\n' } else { ' ' });
    let mut i = 0;
    while i < c.len() {
        let at = |k: usize| c.get(i + k).copied();
        match c[i] {
            '/' if at(1) == Some('/') => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            '/' if at(1) == Some('*') => {
                let mut depth = 0;
                while i < c.len() {
                    if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                    } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        blank(&mut out, c[i]);
                        i += 1;
                    }
                }
            }
            // r"..", r#".."#, br"..": no escapes, closed by " and as many #.
            'r' if (i == 0 || !ident(c[i - 1]) || (c[i - 1] == 'b' && (i < 2 || !ident(c[i - 2]))))
                && matches!(at(1), Some('"' | '#')) =>
            {
                let hashes = c[i + 1..].iter().take_while(|&&h| h == '#').count();
                if c.get(i + 1 + hashes) != Some(&'"') {
                    out.push('r');
                    i += 1;
                    continue;
                }
                out.push_str("r\"");
                i += hashes + 2;
                while i < c.len() {
                    if c[i] == '"' && c[i + 1..].iter().take(hashes).filter(|&&h| h == '#').count() == hashes {
                        i += hashes + 1;
                        break;
                    }
                    blank(&mut out, c[i]);
                    i += 1;
                }
                out.push('"');
            }
            '"' => {
                out.push('"');
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' {
                        blank(&mut out, c[i]);
                        i += 1;
                    }
                    if i < c.len() {
                        blank(&mut out, c[i]);
                        i += 1;
                    }
                }
                out.push('"');
                i += 1;
            }
            // A char literal, or a lifetime.
            '\'' if at(1) == Some('\\') => {
                i += 3;
                while i < c.len() && c[i] != '\'' {
                    i += 1;
                }
                out.push_str("' '");
                i += 1;
            }
            '\'' if at(2) == Some('\'') => {
                out.push_str("' '");
                i += 3;
            }
            ch => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// `code` with every item under `#[cfg(test)]` blanked out.
fn without_tests(code: &str) -> String {
    let mut bytes = code.as_bytes().to_vec();
    let attr = b"#[cfg(test)]";
    let mut from = 0;
    while let Some(off) = bytes[from..].windows(attr.len()).position(|w| w == attr) {
        let start = from + off;
        let mut depth = 0;
        let mut end = bytes.len();
        for (j, b) in bytes.iter().enumerate().skip(start + attr.len()) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = j + 1;
                        break;
                    }
                }
                b';' if depth == 0 => {
                    end = j + 1;
                    break;
                }
                _ => {}
            }
        }
        for b in &mut bytes[start..end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        from = end;
    }
    String::from_utf8(bytes).unwrap()
}

/// `file:line: text` for each line where one of `needles` appears as a
/// whole path or word (no identifier character right before it).
fn find(file: &str, code: &str, needles: &[&str]) -> Vec<String> {
    let mut hits = Vec::new();
    for (n, line) in code.lines().enumerate() {
        for needle in needles {
            let found = line.match_indices(needle).any(|(at, _)| !line[..at].chars().next_back().is_some_and(ident));
            if found {
                hits.push(format!("{file}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    hits
}

#[test]
fn the_lexer_sees_through_comments_and_literals() {
    let src = r##"
/// println!("doc")
fn f<'a>(x: &'a str) -> char {
    /* dbg!(x) /* nested */ eprintln!() */
    let _ = "println!(\"in a string\") {";
    let _ = r#"dbg!("raw") }"#;
    let _ = b'{';
    let _ = '\'';
    let _ = br"print!()";
    print!("{x}"); // eprint!()
    '}'
}
#[cfg(test)]
mod tests {
    fn g() { let _ = "}"; dbg!(1); }
}
#[cfg(test)]
use std::fmt;
fn h() { dbg!(2) }
"##;
    let code = without_tests(&code_only(src));
    assert_eq!(code.lines().count(), src.lines().count());
    let hits = find("x.rs", &code, &["println!", "eprintln!", "print!", "eprint!", "dbg!"]);
    assert_eq!(hits, ["x.rs:10: print!(\"   \");", "x.rs:19: fn h() { dbg!(2) }"]);
    assert!(code.contains("fn f<'a>(x: &'a str)"));
    assert!(!code.contains("fmt"));
}

#[test]
fn never_prints() {
    let mut hits = Vec::new();
    for (file, src) in sources() {
        let code = without_tests(&code_only(&src));
        assert!(!code.contains("mod tests"), "{file}: the test module was not blanked");
        let needles = ["println!", "eprintln!", "print!", "eprint!", "dbg!", "stdout(", "stderr("];
        hits.extend(find(&file, &code, &needles));
    }
    assert!(hits.is_empty(), "prints outside tests:\n{}", hits.join("\n"));
}

#[test]
fn state_logic_never_reads_the_clock() {
    // The poller, the demo and the network clients wait on real time;
    // everything else takes the time it is given.
    let allowed = ["poll.rs", "demo.rs", "nut.rs", "recorder.rs"];
    let sources = sources();
    for must in ["model.rs", "event.rs", "history.rs", "status.rs"] {
        assert!(sources.iter().any(|(f, _)| f == must), "no {must}");
    }
    let mut hits = Vec::new();
    for (file, src) in sources.iter().filter(|(f, _)| !allowed.contains(&f.as_str())) {
        let code = without_tests(&code_only(src));
        hits.extend(find(file, &code, &["SystemTime::now", "Instant::now", "elapsed("]));
    }
    assert!(hits.is_empty(), "clock reads in state logic:\n{}", hits.join("\n"));
}

#[test]
fn depends_on_serde_at_most() {
    // In a packaged crate, Cargo.toml is rewritten and the original kept.
    let orig = root().join("Cargo.toml.orig");
    let toml = fs::read_to_string(if orig.exists() { orig } else { root().join("Cargo.toml") }).unwrap();
    let mut section = String::new();
    let mut deps = Vec::new();
    for line in toml.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if line.starts_with('[') {
            section = line.to_string();
            let other =
                section.contains("dependencies") && section != "[dependencies]" && section != "[dev-dependencies]";
            assert!(!other, "dependencies outside [dependencies]: {section}");
            continue;
        }
        if section == "[dependencies]" {
            deps.push(line.to_string());
        }
    }
    let names: Vec<&str> = deps.iter().map(|d| d.split(['=', '.']).next().unwrap().trim()).collect();
    assert_eq!(names, ["serde"], "{deps:?}");
    assert!(deps[0].replace(' ', "").contains("optional=true"), "{}", deps[0]);
    let features = toml.split("[features]").nth(1).expect("a [features] section");
    assert!(features.contains("\"dep:serde\""), "the serde feature should enable dep:serde");
}

#[test]
fn forbids_unsafe_code() {
    let lib = fs::read_to_string(root().join("src/lib.rs")).unwrap();
    let code = code_only(&lib);
    assert!(code.contains("#![forbid(unsafe_code)]"));
    assert!(code.contains("#![warn(missing_docs)]"));
}
