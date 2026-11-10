# Changelog

## Unreleased

- The Load card's outlet list is no longer capped at eight: at natural height every outlet is listed, and a card shrunk by a short terminal fits as many as its rows allow, ending with how many more the Variables view has.

## 0.1.2 - 2026-11-09

- The Load card lists the outlets or outlet groups a device reports, with each one's draw and its share of the total, for PDUs, rack UPSes with switchable groups and power stations such as the EcoFlow (AC, DC, USB-A, USB-C).

## 0.1.1 - 2026-11-09

- A connection loss in recorded history is now followed by the "Reconnected" event, so a past upsd restart no longer looks like an ongoing outage.
- The Device card shows the rated watts next to an EcoFlow pack size, once the driver reports them.
- The README notes how EcoFlow power stations report live power through NUT's `ecoflow_cdc_port`.

## 0.1.0 - 2026-11-09

The first release.

- A dashboard for every UPS on one or more upsd servers, with Overview, History, Variables and Events views.
  The layout follows the terminal, from three columns of cards down to a few lines in a small split.
- Five charge styles: a battery gauge, seven-segment digits, LED segments, pixel digits and a plain bar; `b` switches between them.
- Estimates of the time until low battery or full from the observed charge rate, with the power draw when the capacity is known.
- `upst record`, a service that keeps a journal of every change and serves it, so upst opens with the last day of charts and events.
  It comes with a NixOS module and systemd units.
- A TOML config file for the whole machine and per user, with battery capacities per UPS.
- Instant commands and writable variables, with typed confirmation for anything that can cut power.
- `--once` and `--json` for scripts and status bars, and `--demo` with a simulated upsd.
- Binaries for Linux (x86_64 and aarch64, statically linked) and macOS (Intel and Apple silicon).
