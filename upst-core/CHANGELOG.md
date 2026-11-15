# Changelog

Changes to the public API of upst-core. Until 1.0, breaking changes come with a new minor version.

## 0.1.0 - 2026-11-15

The first release: the core of upst 0.1.2, split out of the binary.

- `nut::Client`: a blocking upsd client with timeouts on connect, read and write, listing, descriptions
  (`var_info`), login, instant commands, SET VAR and result tracking. `nut::Error` has the upsd code,
  `is_fatal()`, `is_auth()` and `source()`; no error or `Debug` output contains a password. An argument
  with a line break is refused (`Error::Argument`) instead of being sent as two requests.
- `Ups`: the state of a UPS, built with `Ups::builder(name, server).capacity_wh(..).history(..)`, with
  getters for its variables, status, link, outage statistics and capacity. `apply` and `lost` take the
  time from the caller and return typed `Event`s (`EventKind`, `Severity`, a stable `code()` and the
  English text as `Display`); `gap` marks time nothing is known about. The history is optional;
  `estimate(at)` works without it.
- `Ups::replay` rebuilds a UPS and its events from a recorder journal. Unlike upst 0.1.2, it applies a
  poll before a lost connection recorded in the same second, and goes on with the polls after a silence
  that had no recorder restart (a host that slept).
- `poll::Poller`: a thread per upsd server that polls, reconnects with backoff, describes variables and
  runs commands, reporting through a callback or a channel; `set_interval`, `pause`, `refresh` and
  `stop`.
- `journal` (the `upst journal 1` format and its writer, which now drops a half-written last line instead
  of finishing it) and, on Unix, `recorder::Client` for `upst record`.
- `status::flag_info` with a code per flag, `fmt::unit`, `fmt::parse_num`, `history::recordable` and
  `nut::dangerous`.
- Features `serde` and `demo` (`demo::Server`, a simulated upsd that stops when dropped, and
  `demo::simulate`).
