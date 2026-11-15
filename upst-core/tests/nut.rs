//! The NUT client and the recorder client against scripted fake servers,
//! and against the demo upsd (feature `demo`).

use std::error::Error as _;
use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use upst_core::nut::{Client, DEFAULT_PORT, Error, Tracking, VarInfo, quote, tokenize};
use upst_core::target::{Server, Target, parse_server};

const TIMEOUT: Duration = Duration::from_secs(2);

/// What a fake server does once its script is played out.
#[derive(Clone, Copy, PartialEq)]
enum Then {
    /// Keep reading (and recording) until the client hangs up.
    Hold,
    /// Hang up at once.
    Close,
}

type Conn = (Box<dyn BufRead + Send>, Box<dyn Write + Send>);

/// A server that takes one connection and answers each request line with
/// the scripted reply, recording every line it gets.
struct Fake {
    thread: JoinHandle<Vec<String>>,
    expected: Vec<String>,
}

impl Fake {
    fn start(script: &[(&str, &str)], then: Then, accept: impl FnOnce() -> io::Result<Conn> + Send + 'static) -> Fake {
        let expected = script.iter().map(|(req, _)| req.to_string()).collect();
        let script: Vec<(String, String)> = script.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let thread = thread::spawn(move || {
            let (mut r, mut w) = accept().expect("the client never connected");
            play(&mut r, &mut w, &script, then)
        });
        Fake { thread, expected }
    }

    /// Every line the client sent, once the connection is over.
    fn seen(self) -> Vec<String> {
        self.thread.join().expect("the fake server panicked")
    }

    /// Check that the client sent exactly the scripted requests.
    fn check(self) {
        let expected = self.expected.clone();
        assert_eq!(self.seen(), expected);
    }
}

/// An empty reply says nothing; anything else goes out with a newline.
fn play(r: &mut dyn BufRead, w: &mut dyn Write, script: &[(String, String)], then: Then) -> Vec<String> {
    let mut seen = Vec::new();
    let mut read = |seen: &mut Vec<String>| {
        let mut line = String::new();
        match r.read_line(&mut line) {
            Ok(n) if n > 0 => {
                seen.push(line.trim_end_matches(['\n', '\r']).to_string());
                true
            }
            _ => false,
        }
    };
    for (_, reply) in script {
        if !read(&mut seen) {
            return seen;
        }
        if !reply.is_empty() && w.write_all(format!("{reply}\n").as_bytes()).is_err() {
            return seen;
        }
    }
    if then == Then::Hold {
        while read(&mut seen) {}
    }
    seen
}

/// A fake upsd on a loopback port.
fn upsd(script: &[(&str, &str)], then: Then) -> (Fake, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let fake = Fake::start(script, then, move || {
        let (s, _) = listener.accept()?;
        // Never hang a test for good.
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
        Ok((Box::new(BufReader::new(s.try_clone()?)), Box::new(s)))
    });
    (fake, port)
}

/// A fake upsd and a client connected to it.
fn session(script: &[(&str, &str)], then: Then) -> (Fake, Client) {
    let (fake, port) = upsd(script, then);
    (fake, Client::connect("127.0.0.1", port, TIMEOUT).unwrap())
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

fn io_kind(e: &Error) -> Option<io::ErrorKind> {
    match e {
        Error::Io(e) => Some(e.kind()),
        _ => None,
    }
}

#[test]
fn lists_ups_with_quoted_descriptions() {
    let (fake, mut c) = session(
        &[(
            "LIST UPS",
            "BEGIN LIST UPS\r\n\
             UPS rack \"Rack UPS in the \\\"server\\\" room\"\r\n\
             UPS bare\r\n\
             UPS esc \"back\\\\slash  and  spaces\"\r\n\
             UPS empty \"\"\r\n\
             VAR rack ups.status \"OL\"\r\n\
             END LIST UPS\r",
        )],
        Then::Hold,
    );
    let list = c.list_ups().unwrap();
    assert_eq!(
        list,
        pairs(&[
            ("rack", "Rack UPS in the \"server\" room"),
            ("bare", ""),
            ("esc", "back\\slash  and  spaces"),
            ("empty", ""),
        ])
    );
    drop(c);
    fake.check();
}

#[test]
fn lists_vars_with_quotes_escapes_and_empty_values() {
    let reply = r#"BEGIN LIST VAR ups
VAR ups ups.model "Smart \"UPS\" 1500"
VAR ups device.mfr "Two  spaces  inside"
VAR ups driver.parameter.port "C:\\dev\\usb"
VAR ups battery.type ""
VAR ups ups.status "OL CHRG"
VAR ups ups.load 31
RW ups input.sensitivity "medium"
VAR ups
END LIST VAR ups"#;
    let (fake, mut c) = session(&[("LIST VAR ups", reply), (r#"LIST VAR "my ups""#, "ERR UNKNOWN-UPS")], Then::Hold);
    let vars = c.list_vars("ups").unwrap();
    assert_eq!(
        vars,
        pairs(&[
            ("ups.model", r#"Smart "UPS" 1500"#),
            ("device.mfr", "Two  spaces  inside"),
            ("driver.parameter.port", r"C:\dev\usb"),
            ("battery.type", ""),
            ("ups.status", "OL CHRG"),
            ("ups.load", "31"),
        ])
    );
    // Names that are not plain words are quoted.
    assert_eq!(c.list_vars("my ups").unwrap_err().code(), Some("UNKNOWN-UPS"));
    drop(c);
    fake.check();
}

#[test]
fn lists_rw_and_commands() {
    let (fake, mut c) = session(
        &[
            (
                "LIST RW ups",
                "BEGIN LIST RW ups\nRW ups ups.id \"rack one\"\nRW ups ups.delay.start \"30\"\nEND LIST RW ups",
            ),
            ("LIST CMD ups", "BEGIN LIST CMD ups\nCMD ups beeper.mute\nCMD ups load.off\nEND LIST CMD ups"),
            ("GET CMDDESC ups load.off", "CMDDESC ups load.off \"Turn off the load immediately\""),
            ("GET CMDDESC ups beeper.mute", "CMDDESC ups beeper.mute \"Description unavailable\""),
        ],
        Then::Hold,
    );
    assert_eq!(c.list_rw("ups").unwrap(), pairs(&[("ups.id", "rack one"), ("ups.delay.start", "30")]));
    assert_eq!(c.list_commands("ups").unwrap(), ["beeper.mute", "load.off"]);
    assert_eq!(c.command_description("ups", "load.off").unwrap().as_deref(), Some("Turn off the load immediately"));
    assert_eq!(c.command_description("ups", "beeper.mute").unwrap(), None);
    drop(c);
    fake.check();
}

#[test]
fn err_inside_a_list_is_a_server_error_and_the_connection_goes_on() {
    let (fake, mut c) = session(
        &[
            ("LIST VAR ups", "BEGIN LIST VAR ups\nVAR ups battery.charge \"100\"\nERR DATA-STALE"),
            ("LIST VAR ups", "ERR DRIVER-NOT-CONNECTED"),
            ("VER", "Network UPS Tools upsd 2.8.4"),
        ],
        Then::Hold,
    );
    let e = c.list_vars("ups").unwrap_err();
    assert!(matches!(&e, Error::Server { code, detail: None } if code == "DATA-STALE"), "{e:?}");
    assert!(!e.is_fatal() && !e.is_auth());
    let e = c.list_vars("ups").unwrap_err();
    assert_eq!(e.code(), Some("DRIVER-NOT-CONNECTED"));
    assert!(!e.is_fatal());
    assert_eq!(c.server_version().unwrap(), "Network UPS Tools upsd 2.8.4");
    drop(c);
    fake.check();
}

#[test]
fn server_errors_carry_code_and_detail() {
    let cases: &[(&str, &str, Option<&str>, &str, bool)] = &[
        (
            "ERR ACCESS-DENIED",
            "ACCESS-DENIED",
            None,
            "access denied (check the user's permissions in upsd.users)",
            true,
        ),
        ("ERR INVALID-PASSWORD", "INVALID-PASSWORD", None, "invalid password", true),
        ("ERR INVALID-USERNAME", "INVALID-USERNAME", None, "invalid username", true),
        ("ERR USERNAME-REQUIRED", "USERNAME-REQUIRED", None, "a username is required", true),
        ("ERR PASSWORD-REQUIRED", "PASSWORD-REQUIRED", None, "a password is required", true),
        ("ERR DATA-STALE", "DATA-STALE", None, "data is stale (the driver lost contact with the UPS)", false),
        ("ERR UNKNOWN-UPS", "UNKNOWN-UPS", None, "no such UPS on this server", false),
        (
            "ERR INVALID-ARGUMENT some detail here",
            "INVALID-ARGUMENT",
            Some("some detail here"),
            "invalid argument (some detail here)",
            false,
        ),
        (
            "ERR   VAR-NOT-SUPPORTED   padded  ",
            "VAR-NOT-SUPPORTED",
            Some("padded"),
            "variable not supported (padded)",
            false,
        ),
        ("ERR WEIRD-THING", "WEIRD-THING", None, "WEIRD-THING", false),
        ("ERR WEIRD-THING with words", "WEIRD-THING", Some("with words"), "WEIRD-THING with words", false),
    ];
    let script: Vec<(&str, &str)> = cases.iter().map(|c| ("VER", c.0)).collect();
    let (fake, mut c) = session(&script, Then::Hold);
    for &(_, code, detail, text, auth) in cases {
        let e = c.server_version().unwrap_err();
        match &e {
            Error::Server { code: c, detail: d } => assert_eq!((c.as_str(), d.as_deref()), (code, detail)),
            other => panic!("{other:?}"),
        }
        assert_eq!(e.code(), Some(code));
        assert_eq!(e.to_string(), text);
        assert_eq!(e.is_auth(), auth, "{code}");
        assert!(!e.is_fatal(), "{code}");
        assert!(e.source().is_none());
    }
    drop(c);
    fake.check();
}

#[test]
fn a_reply_without_begin_list_is_a_fatal_protocol_error() {
    let (fake, mut c) = session(&[("LIST UPS", "UPS rack \"Rack\"")], Then::Hold);
    let e = c.list_ups().unwrap_err();
    assert!(matches!(e, Error::Protocol(_)), "{e:?}");
    assert!(e.is_fatal() && !e.is_auth() && e.code().is_none());
    assert!(e.to_string().starts_with("protocol error: expected BEGIN LIST"), "{e}");
    drop(c);
    fake.check();
}

#[test]
fn closing_mid_list_is_a_fatal_eof() {
    let (fake, mut c) = session(&[("LIST VAR ups", "BEGIN LIST VAR ups\nVAR ups battery.charge \"100\"")], Then::Close);
    let e = c.list_vars("ups").unwrap_err();
    assert_eq!(io_kind(&e), Some(io::ErrorKind::UnexpectedEof), "{e:?}");
    assert!(e.is_fatal());
    assert_eq!(e.to_string(), "upsd closed the connection");
    let source = e.source().and_then(|s| s.downcast_ref::<io::Error>()).expect("an io::Error source");
    assert_eq!(source.kind(), io::ErrorKind::UnexpectedEof);
    fake.check();
}

#[test]
fn closing_before_the_answer_is_a_fatal_eof() {
    let (fake, mut c) = session(&[("VER", "")], Then::Close);
    let e = c.server_version().unwrap_err();
    assert_eq!(io_kind(&e), Some(io::ErrorKind::UnexpectedEof), "{e:?}");
    assert!(e.is_fatal());
    fake.check();
}

#[test]
fn silence_times_out() {
    let timeout = Duration::from_millis(300);
    let (fake, port) = upsd(&[("VER", "")], Then::Hold);
    let mut c = Client::connect("127.0.0.1", port, timeout).unwrap();
    let t = Instant::now();
    let e = c.server_version().unwrap_err();
    let took = t.elapsed();
    assert!(took >= Duration::from_millis(250) && took < Duration::from_secs(3), "{took:?}");
    assert!(matches!(io_kind(&e), Some(io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)), "{e:?}");
    assert!(e.is_fatal());
    assert_eq!(e.to_string(), "timed out");
    drop(c);
    fake.check();
}

#[test]
fn connection_refused() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let e = Client::connect("127.0.0.1", port, TIMEOUT).unwrap_err();
    assert_eq!(io_kind(&e), Some(io::ErrorKind::ConnectionRefused), "{e:?}");
    assert!(e.is_fatal());
    assert_eq!(e.to_string(), "connection refused");
    assert!(e.source().is_some());
}

#[test]
fn missing_descriptions_are_none() {
    let (fake, mut c) = session(
        &[
            ("GET DESC ups ups.load", "DESC ups ups.load \"Load on UPS (percent of full)\""),
            ("GET DESC ups ups.foo", "DESC ups ups.foo \"Description unavailable\""),
            ("GET DESC ups ups.bar", "DESC ups ups.bar \"\""),
            ("GET DESC ups ups.baz", "ERR VAR-NOT-SUPPORTED"),
        ],
        Then::Hold,
    );
    assert_eq!(c.var_description("ups", "ups.load").unwrap().as_deref(), Some("Load on UPS (percent of full)"));
    assert_eq!(c.var_description("ups", "ups.foo").unwrap(), None);
    assert_eq!(c.var_description("ups", "ups.bar").unwrap(), None);
    assert_eq!(c.var_description("ups", "ups.baz").unwrap_err().code(), Some("VAR-NOT-SUPPORTED"));
    drop(c);
    fake.check();
}

#[test]
fn var_info_of_an_enum() {
    let (fake, mut c) = session(
        &[
            ("GET DESC ups input.sensitivity", "DESC ups input.sensitivity \"Input power sensitivity\""),
            ("GET TYPE ups input.sensitivity", "TYPE ups input.sensitivity RW ENUM"),
            (
                "LIST ENUM ups input.sensitivity",
                "BEGIN LIST ENUM ups input.sensitivity\n\
                 ENUM ups input.sensitivity \"low\"\n\
                 ENUM ups input.sensitivity \"medium\"\n\
                 ENUM ups input.sensitivity \"very high\"\n\
                 END LIST ENUM ups input.sensitivity",
            ),
        ],
        Then::Hold,
    );
    let info = c.var_info("ups", "input.sensitivity").unwrap();
    assert_eq!(
        info,
        VarInfo {
            desc: Some("Input power sensitivity".into()),
            kind: vec!["RW".into(), "ENUM".into()],
            enum_values: vec!["low".into(), "medium".into(), "very high".into()],
            ranges: vec![],
        }
    );
    assert_eq!(info.max_len(), None);
    drop(c);
    fake.check();
}

#[test]
fn var_info_of_a_range() {
    let (fake, mut c) = session(
        &[
            ("GET DESC ups ups.delay.start", "DESC ups ups.delay.start \"Description unavailable\""),
            ("GET TYPE ups ups.delay.start", "TYPE ups ups.delay.start RW RANGE NUMBER"),
            (
                "LIST RANGE ups ups.delay.start",
                "BEGIN LIST RANGE ups ups.delay.start\n\
                 RANGE ups ups.delay.start \"0\" \"600\"\n\
                 RANGE ups ups.delay.start \"900\" \"1200\"\n\
                 END LIST RANGE ups ups.delay.start",
            ),
        ],
        Then::Hold,
    );
    let info = c.var_info("ups", "ups.delay.start").unwrap();
    assert_eq!(info.desc, None);
    assert_eq!(info.kind, ["RW", "RANGE", "NUMBER"]);
    assert!(info.enum_values.is_empty());
    assert_eq!(info.ranges, pairs(&[("0", "600"), ("900", "1200")]));
    drop(c);
    fake.check();
}

#[test]
fn var_info_falls_back_after_get_desc() {
    let (fake, mut c) = session(
        &[
            ("GET DESC ups ups.id", "DESC ups ups.id \"UPS system identifier\""),
            ("GET TYPE ups ups.id", "TYPE ups ups.id RW STRING:16"),
            ("GET DESC ups ups.x", "DESC ups ups.x \"x\""),
            ("GET TYPE ups ups.x", "ERR FEATURE-NOT-SUPPORTED"),
            ("GET DESC ups ups.y", "DESC ups ups.y \"y\""),
            ("GET TYPE ups ups.y", "TYPE ups ups.y ENUM"),
            ("LIST ENUM ups ups.y", "ERR INVALID-ARGUMENT"),
            ("GET DESC ups ups.z", "ERR VAR-NOT-SUPPORTED"),
        ],
        Then::Hold,
    );
    let info = c.var_info("ups", "ups.id").unwrap();
    assert_eq!(info.kind, ["RW", "STRING:16"]);
    assert_eq!(info.max_len(), Some(16));
    let info = c.var_info("ups", "ups.x").unwrap();
    assert_eq!(info.desc.as_deref(), Some("x"));
    assert!(info.kind.is_empty());
    let info = c.var_info("ups", "ups.y").unwrap();
    assert_eq!(info.kind, ["ENUM"]);
    assert!(info.enum_values.is_empty());
    assert_eq!(c.var_info("ups", "ups.z").unwrap_err().code(), Some("VAR-NOT-SUPPORTED"));
    drop(c);
    fake.check();
}

#[test]
fn set_var_always_quotes_the_value() {
    let (fake, mut c) = session(
        &[
            (r#"SET VAR ups ups.delay.start "30""#, "OK"),
            (r#"SET VAR ups ups.id "rack \"one\"""#, "OK TRACKING 1f2e-33"),
            (r#"SET VAR ups ups.id """#, "OK"),
            (r#"SET VAR ups ups.id "C:\\x""#, "OK"),
            (r#"SET VAR "my ups" ups.id "a""#, "OK"),
            (r#"SET VAR ups ups.delay.start "-5""#, "ERR INVALID-VALUE"),
        ],
        Then::Hold,
    );
    assert_eq!(c.set_var("ups", "ups.delay.start", "30").unwrap(), None);
    assert_eq!(c.set_var("ups", "ups.id", r#"rack "one""#).unwrap().as_deref(), Some("1f2e-33"));
    assert_eq!(c.set_var("ups", "ups.id", "").unwrap(), None);
    assert_eq!(c.set_var("ups", "ups.id", r"C:\x").unwrap(), None);
    assert_eq!(c.set_var("my ups", "ups.id", "a").unwrap(), None);
    assert_eq!(c.set_var("ups", "ups.delay.start", "-5").unwrap_err().code(), Some("INVALID-VALUE"));
    drop(c);
    fake.check();
}

#[test]
fn instcmd_with_and_without_a_value() {
    let (fake, mut c) = session(
        &[
            ("INSTCMD ups beeper.mute", "OK"),
            ("INSTCMD ups test.panel.start 5", "OK TRACKING abc-123"),
            (r#"INSTCMD ups shutdown.stayoff "two words""#, "OK"),
            ("INSTCMD ups load.off", "ERR CMD-NOT-SUPPORTED"),
        ],
        Then::Hold,
    );
    assert_eq!(c.instcmd("ups", "beeper.mute", None).unwrap(), None);
    assert_eq!(c.instcmd("ups", "test.panel.start", Some("5")).unwrap().as_deref(), Some("abc-123"));
    assert_eq!(c.instcmd("ups", "shutdown.stayoff", Some("two words")).unwrap(), None);
    let e = c.instcmd("ups", "load.off", None).unwrap_err();
    assert_eq!((e.code(), e.is_fatal()), (Some("CMD-NOT-SUPPORTED"), false));
    drop(c);
    fake.check();
}

#[test]
fn tracking_results() {
    let (fake, mut c) = session(
        &[
            ("SET TRACKING ON", "OK"),
            ("SET TRACKING ON", "ERR FEATURE-NOT-CONFIGURED"),
            ("GET TRACKING abc", "PENDING"),
            ("GET TRACKING abc", "SUCCESS"),
            ("GET TRACKING abc", "ERR INSTCMD-FAILED"),
            ("GET TRACKING def", "ERR UNKNOWN"),
            ("GET TRACKING abc", "BANANA"),
        ],
        Then::Hold,
    );
    assert!(c.enable_tracking());
    assert!(!c.enable_tracking());
    assert!(matches!(c.tracking("abc").unwrap(), Tracking::Pending));
    assert!(matches!(c.tracking("abc").unwrap(), Tracking::Success));
    match c.tracking("abc").unwrap() {
        Tracking::Failed(e) => assert_eq!(e.to_string(), "the driver could not run the command"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(c.tracking("def").unwrap(), Tracking::Failed(Error::Server { .. })));
    let e = c.tracking("abc").unwrap_err();
    assert!(matches!(e, Error::Protocol(_)) && e.is_fatal(), "{e:?}");
    drop(c);
    fake.check();
}

#[test]
fn server_version_and_logout() {
    let banner = "Network UPS Tools upsd 2.8.4 - https://networkupstools.org/";
    let (fake, mut c) = session(&[("VER", banner), ("LOGOUT", "OK Goodbye")], Then::Close);
    assert_eq!(c.server_version().unwrap(), banner);
    c.logout();
    fake.check();
}

#[test]
fn logout_gives_up_on_a_silent_server() {
    let (fake, port) = upsd(&[("LOGOUT", "")], Then::Hold);
    let c = Client::connect("127.0.0.1", port, Duration::from_millis(300)).unwrap();
    let t = Instant::now();
    c.logout();
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    // Dropping the client closed the connection, or `seen` would wait.
    fake.check();
}

#[test]
fn credentials_never_show_up_in_errors() {
    for code in ["INVALID-PASSWORD", "ACCESS-DENIED"] {
        let reply = format!("ERR {code}");
        let (fake, mut c) = session(&[("USERNAME admin", "OK"), ("PASSWORD s3cret", reply.as_str())], Then::Hold);
        let e = c.authenticate("admin", "s3cret").unwrap_err();
        assert!(e.is_auth() && !e.is_fatal(), "{e:?}");
        for text in [e.to_string(), format!("{e:?}"), format!("{e:#?}"), format!("{c:?}"), format!("{c:#?}")] {
            assert!(!text.contains("s3cret"), "{text}");
        }
        drop(c);
        fake.check();
    }
}

#[test]
fn credentials_are_quoted_and_a_refused_user_gets_no_password() {
    let (fake, mut c) = session(
        &[
            (r#"USERNAME "bad user""#, "ERR INVALID-USERNAME"),
            ("USERNAME admin", "OK"),
            (r#"PASSWORD "pa ss\"w\\rd""#, "OK"),
        ],
        Then::Hold,
    );
    let e = c.authenticate("bad user", "s3cret").unwrap_err();
    assert!(e.is_auth());
    c.authenticate("admin", r#"pa ss"w\rd"#).unwrap();
    drop(c);
    fake.check();
}

#[test]
fn newlines_in_arguments_cannot_inject_requests() {
    let (fake, mut c) = session(&[(r#"SET VAR ups ups.id "x""#, "OK")], Then::Hold);
    // Whether the client refuses the value or escapes it, upsd must not
    // see a second request.
    let _ = c.set_var("ups", "ups.id", "x\nINSTCMD ups load.off \"");
    drop(c);
    let seen = fake.seen();
    assert!(seen.len() <= 1 && !seen.iter().any(|l| l.starts_with("INSTCMD")), "{seen:?}");
}

#[test]
fn line_breaks_are_refused_before_anything_is_sent() {
    let (fake, mut c) = session(&[("VER", "Network UPS Tools upsd 2.8.4")], Then::Hold);
    for value in ["a\nb", "a\rb", "\n"] {
        let e = c.set_var("ups", "ups.id", value).unwrap_err();
        assert!(matches!(e, Error::Argument(_)) && !e.is_fatal(), "{e:?}");
        assert_eq!(e.to_string(), "invalid argument: line break in an argument");
    }
    assert!(matches!(c.instcmd("ups\n", "beeper.mute", None), Err(Error::Argument(_))));
    assert!(matches!(c.list_vars("a\rb"), Err(Error::Argument(_))));
    // Nothing went out, and the connection still works.
    assert_eq!(c.server_version().unwrap(), "Network UPS Tools upsd 2.8.4");
    drop(c);
    fake.check();
}

#[test]
fn var_info_passes_on_a_broken_connection() {
    let (fake, mut c) = session(&[("GET DESC ups ups.id", "DESC ups ups.id \"UPS system identifier\"")], Then::Close);
    let e = c.var_info("ups", "ups.id").unwrap_err();
    assert!(e.is_fatal(), "{e:?}");
    drop(c);
    fake.check();
}

#[test]
fn a_zero_timeout_means_none() {
    let (fake, port) = upsd(&[("VER", "Network UPS Tools upsd 2.8.4")], Then::Hold);
    let mut c = Client::connect("127.0.0.1", port, Duration::ZERO).unwrap();
    assert_eq!(c.server_version().unwrap(), "Network UPS Tools upsd 2.8.4");
    drop(c);
    fake.check();
}

#[test]
fn io_errors_have_a_source() {
    let e = Error::from(io::Error::new(io::ErrorKind::BrokenPipe, "gone"));
    assert!(e.is_fatal() && e.code().is_none() && !e.is_auth());
    let source = e.source().and_then(|s| s.downcast_ref::<io::Error>()).unwrap();
    assert_eq!(source.kind(), io::ErrorKind::BrokenPipe);
    assert!(Error::Protocol("x".into()).source().is_none());
}

/// xorshift, so the properties need no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// Up to 11 characters, often ones that need quoting.
    fn string(&mut self) -> String {
        const CHARS: &[char] = &[
            'a', 'Z', '7', '.', '-', '_', ':', '/', '@', '+', ' ', ' ', '\t', '\n', '\r', '"', '"', '\\', '\\', '\'',
            '#', '=', '{', 'é', 'ß', '日', '😀', '\u{a0}', '\u{2028}', '\0',
        ];
        let len = self.below(12);
        (0..len).map(|_| CHARS[self.below(CHARS.len())]).collect()
    }
}

const SEEDS: [u64; 4] = [1, 42, 0x9e37_79b9_7f4a_7c15, 0xdead_beef_cafe_f00d];

#[test]
fn quote_then_tokenize_gives_the_word_back() {
    for seed in SEEDS {
        let mut rng = Rng(seed);
        for _ in 0..2000 {
            let s = rng.string();
            let q = quote(&s);
            assert_eq!(tokenize(&q), std::slice::from_ref(&s), "seed {seed}: {s:?} quoted as {q:?}");
            if !q.starts_with('"') {
                assert_eq!(q, s, "only plain words go out unquoted");
            }
        }
    }
}

#[test]
fn quoted_arguments_split_back_apart() {
    const GAPS: [&str; 4] = [" ", "  ", "\t", " \t "];
    for seed in SEEDS {
        let mut rng = Rng(seed);
        for _ in 0..500 {
            let args: Vec<String> = (0..1 + rng.below(6)).map(|_| rng.string()).collect();
            let mut line = GAPS[rng.below(GAPS.len())].to_string();
            for a in &args {
                line.push_str(&quote(a));
                line.push_str(GAPS[rng.below(GAPS.len())]);
            }
            assert_eq!(tokenize(&line), args, "seed {seed}: {line:?}");
        }
    }
}

#[test]
fn servers_round_trip_through_display() {
    let hosts = [
        "localhost",
        "nas",
        "nas.lan",
        "ups-1.example.org",
        "UPPER.case",
        "192.168.1.10",
        "127.0.0.1",
        "::1",
        "::",
        "fe80::1",
        "fe80::1%eth0",
        "2001:db8::42:1",
        "2001:0db8:0000:0000:0000:ff00:0042:8329",
        "::ffff:192.0.2.1",
    ];
    for host in hosts {
        for port in [DEFAULT_PORT, 1, 3494, 8080, 65535] {
            let s = Server { host: host.into(), port };
            let text = s.to_string();
            assert_eq!(parse_server(&text, DEFAULT_PORT), Ok(s.clone()), "{text}");
            assert_eq!(text.ends_with(&format!(":{port}")), port != DEFAULT_PORT, "{text}");
            assert_eq!(text.starts_with('['), host.contains(':'), "{text}");
            for ups in [Some("ups"), Some("rack-1"), Some("my_ups"), Some("eco.flow"), None] {
                let t = Target { ups: ups.map(String::from), server: s.clone() };
                let text = t.to_string();
                assert_eq!(text.parse::<Target>(), Ok(t.clone()), "{text}");
            }
        }
    }
}

#[test]
fn targets_parse_to_their_canonical_form() {
    let cases = [
        ("ups", "ups@localhost"),
        ("  ups@nas  ", "ups@nas"),
        ("ups@nas:3493", "ups@nas"),
        ("ups@nas:3494", "ups@nas:3494"),
        ("ups@[::1]", "ups@[::1]"),
        ("ups@[::1]:3493", "ups@[::1]"),
        ("ups@::1", "ups@[::1]"),
        ("ups@[fe80::1%eth0]:4000", "ups@[fe80::1%eth0]:4000"),
        ("@nas", "@nas"),
        ("*@nas:3500", "@nas:3500"),
        ("@", "@localhost"),
        ("ups@", "ups@localhost"),
        ("ups@:3500", "ups@localhost:3500"),
    ];
    for (input, canonical) in cases {
        let t: Target = input.parse().unwrap_or_else(|e| panic!("{input:?}: {e}"));
        assert_eq!(t.to_string(), canonical, "{input:?}");
    }
    assert_eq!(parse_server("nas", 3494).unwrap().port, 3494);
    assert_eq!(parse_server("", 3494), Ok(Server { host: "localhost".into(), port: 3494 }));
}

#[test]
fn targets_reject_garbage() {
    for bad in [
        "",
        "   ",
        "ups@host:",
        "ups@host:http",
        "ups@host:65536",
        "ups@host:-1",
        "ups@host: 1",
        "ups@[::1",
        "ups@[::1]x",
        "ups@[::1]:",
        "ups@[::1]:abc",
        "a b@host",
        "a\"b@host",
        "ups\t@host",
    ] {
        assert!(bad.parse::<Target>().is_err(), "{bad:?} parsed as {:?}", bad.parse::<Target>());
    }
}

#[cfg(unix)]
mod recorder {
    use std::fs;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use upst_core::journal::{Kind, Record};
    use upst_core::recorder::{Addr, Client, Listed};

    use super::*;

    /// A directory of its own, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("upst-core-nut-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A fake recorder on a Unix socket.
    fn unix(script: &[(&str, &str)], then: Then) -> (Fake, Addr, TempDir) {
        let dir = TempDir::new();
        let path = dir.0.join("upst.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let fake = Fake::start(script, then, move || {
            let (s, _) = listener.accept()?;
            s.set_read_timeout(Some(Duration::from_secs(10)))?;
            Ok((Box::new(BufReader::new(s.try_clone()?)), Box::new(s)))
        });
        (fake, Addr::Unix(path), dir)
    }

    fn tcp(script: &[(&str, &str)], then: Then) -> (Fake, Addr) {
        let (fake, port) = upsd(script, then);
        (fake, Addr::Tcp(Server { host: "127.0.0.1".into(), port }))
    }

    const LIST: &str = "BEGIN LIST
UPS ups@localhost 1762646400 1762650000
UPS rack@nas:3500 1762646400.5 1762650000 with new fields at the end
junk that is not a journal
UPS short
UPS later@nas notatime 1762650000
END LIST";

    fn listed() -> Vec<Listed> {
        vec![
            Listed { label: "ups@localhost".into(), first: 1762646400.0, last: 1762650000.0 },
            Listed { label: "rack@nas:3500".into(), first: 1762646400.5, last: 1762650000.0 },
        ]
    }

    #[test]
    fn version_list_and_quit_over_a_unix_socket() {
        let (fake, addr, _dir) =
            unix(&[("VER", "upst 0.1.2 journal 1"), ("LIST", LIST), ("QUIT", "OK Goodbye")], Then::Close);
        let mut c = Client::connect(&addr, TIMEOUT).unwrap();
        assert_eq!(c.version().unwrap(), "upst 0.1.2 journal 1");
        assert_eq!(c.list().unwrap(), listed());
        c.quit();
        fake.check();
    }

    #[test]
    fn version_and_list_over_tcp() {
        let (fake, addr) = tcp(&[("VER", "upst 0.1.2 journal 1"), ("LIST", LIST), ("QUIT", "OK Goodbye")], Then::Close);
        let mut c = Client::connect(&addr, TIMEOUT).unwrap();
        assert_eq!(c.version().unwrap(), "upst 0.1.2 journal 1");
        assert_eq!(c.list().unwrap(), listed());
        c.quit();
        fake.check();
    }

    #[test]
    fn get_reads_journal_lines_with_escapes() {
        let reply = [
            "BEGIN ups@localhost",
            "# upst journal 1 ups@localhost",
            "1762646400 !start",
            "1762646400 !up",
            "1762646400 ups.status OL",
            r"1762646401 ups.alarm line one\nline two\r",
            r"1762646402 driver.parameter.port C:\\dev\\usb",
            "1762646402 battery.type ",
            "1762646403 -ups.alarm",
            "1762646404 !down Connection refused",
            "1762646405 !marker-from-the-future",
            "garbage",
            "1762646406 !nodata DATA-STALE",
            "END",
        ]
        .join("\n");
        let (fake, addr, _dir) = unix(&[("GET ups@localhost 1762646400", &reply)], Then::Hold);
        let mut c = Client::connect(&addr, TIMEOUT).unwrap();
        let records = c.get("ups@localhost", 1762646400.9).unwrap();
        let set = |t: f64, k: &str, v: &str| Record::new(t, Kind::Set(k.into(), v.into()));
        assert_eq!(
            records,
            [
                Record::new(1762646400.0, Kind::Start),
                Record::new(1762646400.0, Kind::Up),
                set(1762646400.0, "ups.status", "OL"),
                set(1762646401.0, "ups.alarm", "line one\nline two\r"),
                set(1762646402.0, "driver.parameter.port", r"C:\dev\usb"),
                set(1762646402.0, "battery.type", ""),
                Record::new(1762646403.0, Kind::Unset("ups.alarm".into())),
                Record::new(1762646404.0, Kind::Down("Connection refused".into())),
                Record::new(1762646406.0, Kind::NoData("DATA-STALE".into())),
            ]
        );
        drop(c);
        fake.check();
    }

    #[test]
    fn errors_from_the_recorder() {
        let (fake, addr, _dir) = unix(
            &[
                ("GET nope 0", "ERR UNKNOWN-UPS"),
                ("LIST", "OK"),
                ("GET ups@localhost 100", "BEGIN ups@localhost\n100 !start"),
            ],
            Then::Close,
        );
        let mut c = Client::connect(&addr, TIMEOUT).unwrap();
        let e = c.get("nope", 0.0).unwrap_err();
        assert!(e.to_string().contains("UNKNOWN-UPS"), "{e}");
        let e = c.list().unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
        let e = c.get("ups@localhost", 100.0).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "{e}");
        fake.check();
    }

    #[test]
    fn eof_in_the_middle_of_a_list() {
        let (fake, addr, _dir) = unix(&[("LIST", "BEGIN LIST\nUPS ups@localhost 1 2")], Then::Close);
        let mut c = Client::connect(&addr, TIMEOUT).unwrap();
        assert_eq!(c.list().unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        fake.check();
    }

    #[test]
    fn a_silent_recorder_times_out() {
        let (fake, addr, _dir) = unix(&[("VER", "")], Then::Hold);
        let mut c = Client::connect(&addr, Duration::from_millis(300)).unwrap();
        let t = Instant::now();
        let e = c.version().unwrap_err();
        assert!(matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut), "{e}");
        assert!(t.elapsed() < Duration::from_secs(3));
        drop(c);
        fake.check();
    }

    #[test]
    fn labels_that_cannot_travel_are_refused() {
        let (fake, addr) = tcp(&[], Then::Hold);
        let mut c = Client::connect(&addr, TIMEOUT).unwrap();
        for label in ["", "ups@host\nLIST", "two words", "a\rb"] {
            assert_eq!(c.get(label, 0.0).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{label:?}");
        }
        drop(c);
        assert!(fake.seen().is_empty());
    }

    #[test]
    fn a_missing_socket_is_an_error() {
        let dir = TempDir::new();
        let addr = Addr::Unix(dir.0.join("none.sock"));
        assert_eq!(Client::connect(&addr, TIMEOUT).unwrap_err().kind(), io::ErrorKind::NotFound);
    }
}

#[cfg(feature = "demo")]
mod demo_server {
    use std::io::Read;
    use std::net::TcpStream;
    use std::sync::{Arc, Barrier};

    use upst_core::demo;

    use super::*;

    fn client(server: &demo::Server) -> Client {
        Client::connect("127.0.0.1", server.addr().port(), TIMEOUT).unwrap()
    }

    #[test]
    fn sees_rack_and_pack() {
        let server = demo::Server::spawn().unwrap();
        let mut c = client(&server);
        assert_eq!(
            c.list_ups().unwrap(),
            pairs(&[("rack", "Simulated rack UPS"), ("pack", "Simulated power station")])
        );
        assert!(c.server_version().unwrap().starts_with("Network UPS Tools upsd 2.8.4 (upst demo)"));
        c.logout();
    }

    #[test]
    fn lists_everything() {
        let server = demo::Server::spawn().unwrap();
        let mut c = client(&server);
        for (ups, _) in c.list_ups().unwrap() {
            let vars = c.list_vars(&ups).unwrap();
            for k in ["ups.status", "battery.charge", "battery.runtime", "device.model"] {
                assert!(vars.iter().any(|(n, _)| n == k), "{ups} has no {k}");
            }
            for (var, value) in c.list_rw(&ups).unwrap() {
                assert!(vars.contains(&(var.clone(), value.clone())), "{ups} {var}");
                let info = c.var_info(&ups, &var).unwrap();
                assert!(info.kind.iter().any(|k| k == "RW"), "{ups} {var}: {info:?}");
                assert!(!info.enum_values.is_empty() || !info.ranges.is_empty(), "{ups} {var}: {info:?}");
                if !info.enum_values.is_empty() {
                    assert!(info.enum_values.contains(&value), "{ups} {var}={value}: {info:?}");
                }
            }
            let cmds = c.list_commands(&ups).unwrap();
            assert!(!cmds.is_empty());
            for cmd in cmds {
                assert!(c.command_description(&ups, &cmd).unwrap().is_some(), "{ups} {cmd}");
            }
        }
        let info = c.var_info("rack", "ups.status").unwrap();
        assert_eq!(info.desc.as_deref(), Some("UPS status"));
        assert!(!info.kind.iter().any(|k| k == "RW"));
        c.logout();
    }

    #[test]
    fn runs_a_tracked_command_after_login() {
        let server = demo::Server::spawn().unwrap();
        let mut c = client(&server);
        assert_eq!(c.instcmd("rack", "beeper.mute", None).unwrap_err().code(), Some("USERNAME-REQUIRED"));
        c.authenticate("admin", "s3cret").unwrap();
        assert!(c.enable_tracking());
        let id = c.instcmd("rack", "beeper.mute", None).unwrap().expect("a tracking id");
        let until = Instant::now() + Duration::from_secs(5);
        let mut pending = 0;
        loop {
            match c.tracking(&id).unwrap() {
                Tracking::Success => break,
                Tracking::Pending if Instant::now() < until => {
                    pending += 1;
                    thread::sleep(Duration::from_millis(100));
                }
                other => panic!("{other:?} after {pending} pending"),
            }
        }
        assert!(pending > 0, "the demo answers PENDING for a while first");
        assert!(c.list_vars("rack").unwrap().contains(&("ups.beeper.status".into(), "muted".into())));
        c.logout();
    }

    #[test]
    fn a_wrong_password_is_denied() {
        let server = demo::Server::spawn().unwrap();
        let mut c = client(&server);
        c.authenticate("admin", "wrong").unwrap();
        let e = c.instcmd("rack", "beeper.mute", None).unwrap_err();
        assert!(e.is_auth() && !e.is_fatal(), "{e:?}");
        assert!(!format!("{e} {e:?} {c:?}").contains("wrong"));
    }

    /// A raw connection blocked on a read, reporting how the read ended.
    fn blocked_reader(server: &demo::Server) -> std::sync::mpsc::Receiver<io::Result<usize>> {
        let mut s = TcpStream::connect(server.addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0; 64];
            let _ = tx.send(s.read(&mut buf));
        });
        rx
    }

    fn assert_closed(rx: std::sync::mpsc::Receiver<io::Result<usize>>) {
        match rx.recv_timeout(Duration::from_secs(3)).expect("the read is still blocked") {
            Ok(n) => assert_eq!(n, 0, "the demo said something unprompted"),
            Err(e) => assert!(!matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut), "{e}"),
        }
    }

    #[test]
    fn stop_closes_connections_and_frees_the_port() {
        let server = demo::Server::spawn().unwrap();
        let addr = server.addr();
        let blocked = blocked_reader(&server);
        let mut idle = client(&server);
        idle.list_ups().unwrap();
        let t = Instant::now();
        server.stop();
        assert!(t.elapsed() < Duration::from_secs(2), "stop took {:?}", t.elapsed());
        assert_closed(blocked);
        let e = idle.list_ups().unwrap_err();
        assert!(e.is_fatal(), "{e:?}");
        let refused = TcpStream::connect_timeout(&addr, Duration::from_secs(1)).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);
        TcpListener::bind(addr).expect("the port is free again");
    }

    #[test]
    fn dropping_the_server_stops_it() {
        let server = demo::Server::spawn().unwrap();
        let addr = server.addr();
        let blocked = blocked_reader(&server);
        drop(server);
        assert_closed(blocked);
        let refused = TcpStream::connect_timeout(&addr, Duration::from_secs(1)).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);
    }

    #[test]
    fn serves_many_clients_at_once() {
        let server = demo::Server::spawn().unwrap();
        let port = server.addr().port();
        let n = 24;
        let start = Arc::new(Barrier::new(n));
        let threads: Vec<_> = (0..n)
            .map(|i| {
                let start = start.clone();
                thread::spawn(move || {
                    let mut c = Client::connect("127.0.0.1", port, Duration::from_secs(5)).unwrap();
                    start.wait();
                    for _ in 0..5 {
                        assert_eq!(c.list_ups().unwrap().len(), 2);
                        let ups = if i % 2 == 0 { "rack" } else { "pack" };
                        let vars = c.list_vars(ups).unwrap();
                        assert!(vars.iter().any(|(k, _)| k == "battery.charge"));
                        assert!(c.var_description(ups, "battery.charge").unwrap().is_some());
                    }
                    c.logout();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        server.stop();
    }

    #[test]
    fn stop_does_not_wait_for_a_client_that_never_reads() {
        let server = demo::Server::spawn().unwrap();
        let mut s = TcpStream::connect(server.addr()).unwrap();
        s.set_write_timeout(Some(Duration::from_secs(1))).unwrap();
        // Far more answers than the socket buffers hold, and none read.
        let _ = s.write_all("LIST VAR rack\n".repeat(4000).as_bytes());
        let t = Instant::now();
        server.stop();
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    #[test]
    fn history_needs_a_sensible_step() {
        for (secs, step) in [(3600.0, 0.0), (3600.0, -1.0), (-1.0, 1.0), (f64::NAN, 1.0), (1e12, 1.0)] {
            let e = demo::Server::spawn_with_history(secs, step).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{secs} {step}");
        }
        let (_server, past) = demo::Server::spawn_with_history(10.0, 5.0).unwrap();
        assert!(past.iter().all(|(_, polls)| polls.len() == 2));
    }
}
