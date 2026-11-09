# Setting up upst

This page covers what upst needs, getting upsd ready for it, the config file, and running `upst record` so upst opens with history.
For what upst shows and its keys, see the [README](../README.md).

- [Requirements](#requirements)
- [Install](#install)
- [Prepare upsd](#prepare-upsd)
- [The config file](#the-config-file)
- [Run the recorder](#run-the-recorder)
- [Check that it works](#check-that-it-works)
- [Troubleshooting](#troubleshooting)

## Requirements

- **A NUT server**: `upsd` from [Network UPS Tools](https://networkupstools.org/) with the driver for your UPS running (upst is tested with NUT 2.8).
  upst speaks the network protocol itself, so the machine it runs on needs no NUT packages.
  From NUT 2.8 on, upsd also reports whether an instant command actually ran.
- **Linux or macOS**, both tested in CI. The BSDs should work but are untested; Windows is not supported.
- **A terminal** with UTF-8.
  upst uses the 16 standard colors, so it follows the terminal's light or dark theme.
  The charts use braille and block characters and the battery gauge uses box-drawing ones; common monospace fonts have them all.
  On the Linux console, or with a font that lacks them, use `--ascii`.
  Any size works: from 132 columns up it shows three columns of cards, and it goes down to a few borderless lines in a small split.
- **To build it**: Rust 1.88 or newer, or Nix with flakes. It needs no C libraries.
- **For the recorder**: a service manager to keep it running (a systemd unit and a NixOS module are included), a writable directory for the journals and, for upst on the same machine, a directory for its socket.

## Install

Prebuilt binaries for Linux (x86_64 and aarch64, static) and macOS are on the [releases page](https://github.com/Dolfost/upst/releases/latest):

```sh
curl -L https://github.com/Dolfost/upst/releases/latest/download/upst-x86_64-unknown-linux-musl.tar.gz | tar xz
sudo install upst-*/upst /usr/local/bin/
```

That path is where [contrib/upst.service](../contrib/upst.service) expects it.

With Nix:

```sh
nix run github:Dolfost/upst -- --demo   # try it
nix profile install github:Dolfost/upst
```

On NixOS, the [module](#on-nixos) installs upst along with the recorder.

With Cargo:

```sh
cargo install --git https://github.com/Dolfost/upst
```

That puts `upst` in `~/.cargo/bin`.
`upst --demo` runs a simulated upsd, to see upst without touching a real one.

## Prepare upsd

**On the same machine**, upsd normally listens on `localhost:3493` already, and `upst` with no arguments watches every UPS there.

**From another machine**, upsd has to listen on an address that machine can reach.
In `upsd.conf`:

```text
LISTEN 0.0.0.0 3493
```

or on NixOS:

```nix
power.ups.upsd.listen = [ { address = "0.0.0.0"; port = 3493; } ];
```

Then restart upsd, let TCP port 3493 through the firewall, and run `upst ups@that-host`.

**Watching needs no login**, the same as `upsc`.
Instant commands and changing writable variables need an upsd user with the rights for them; the README shows [how to add one](../README.md#instant-commands-and-writable-variables).

## The config file

Every long option of upst and of `upst record` can be set in a TOML file, so a plain `upst` starts the way you like it.

### Where it is read from

1. `/etc/upst/config.toml`: for everyone on the machine. The NixOS module writes this one.
2. `~/.config/upst/config.toml` (or `$XDG_CONFIG_HOME/upst/config.toml`): yours, which wins over the first key by key.

Both are optional.
`--config FILE` (or `UPST_CONFIG=FILE`) reads only that file instead; `--config /dev/null` ignores both.
The command line wins over the files, and so do the environment variables (`UPST_TARGETS`, `UPST_CHARGE_STYLE` and the others).

[contrib/config.toml](../contrib/config.toml) lists every key, commented out, and is a good starting point:

```sh
mkdir -p ~/.config/upst
cp contrib/config.toml ~/.config/upst/
```

### Keys

Keys are named after the long options: `--charge-style lcd` is `charge-style = "lcd"`, and a flag like `--bell` is `bell = true`.
Times are strings with a unit, `ms`, `s`, `m`, `h` or `d` (`"500ms"`, `"24h"`), or a number of seconds.
Paths should be absolute.

| Key | Type | Default | |
|---|---|---|---|
| `targets` | list of strings | every UPS on localhost | UPSes to watch: `"ups@host:port"`, or `"@host"` for every UPS on a server. |
| `interval` | time | `"2s"` | Time between polls. |
| `history` | time | `"24h"` | How much history to keep for charts and estimates, and to load from the recorder. |
| `timeout` | time | `"5s"` | Connect and reply timeout. |
| `view` | string | `"overview"` | View to start on: `overview`, `history`, `variables` or `events`. |
| `charge-style` | string | `"battery"` | `battery`, `lcd`, `segments`, `blocks` or `bar`. |
| `capacity` | number | | Battery capacity in Wh, for Wh left and power draw. |
| `ascii` | bool | `false` | ASCII bars, sparklines and charts. |
| `no-color` | bool | `false` | No colors. |
| `no-mouse` | bool | `false` | Leave the mouse to the terminal. |
| `bell` | bool | `false` | Ring the terminal bell on power events. |
| `format` | string | | Line template for `--once`, as for `--format`. |
| `user` | string | | upsd user for instant commands and writable variables. |
| `password-file` | path | | File whose first line is that user's password. |
| `recorder` | string | `/run/upst/upst.sock`, then `$XDG_RUNTIME_DIR/upst/upst.sock` | The `upst record` to load history from: a socket path or `host[:port]`. |
| `no-recorder` | bool | `false` | Do not load recorded history. |

`--once`, `--json` and `--demo` change what upst does rather than how it looks, so they stay on the command line.

**Per UPS**, a `[ups."<target>"]` table sets a UPS's own battery capacity, which wins over `capacity` unless `--capacity` is given.
The name is written as a target, `ups@host` or just `ups` for one on localhost, and needs the quotes:

| Key | Type | |
|---|---|---|
| `capacity` | number | Battery capacity in Wh. |

**For the recorder**, the `[record]` table holds the options of `upst record`.
`targets` and `timeout` default to the top-level ones, so a recorder records what upst watches unless told otherwise.

| Key | Type | Default | |
|---|---|---|---|
| `targets` | list of strings | top-level `targets`, else every UPS on localhost | UPSes to record. |
| `interval` | time | `"5s"` | Time between polls. |
| `keep` | time | `"7d"` | How long to keep history. Older entries are dropped hourly. |
| `dir` | path | `$STATE_DIRECTORY`, else `$XDG_STATE_HOME/upst` or `~/.local/state/upst` | Where the journals go. |
| `listen` | list of strings | `$RUNTIME_DIRECTORY/upst.sock`, else `$XDG_RUNTIME_DIR/upst/upst.sock` | Socket paths or `host:port` addresses to serve on. A list replaces the default socket, so include it if upst on this machine should find the recorder. |
| `timeout` | time | top-level `timeout`, else `"5s"` | Connect and reply timeout. |

### An example

Two UPSes on a NAS and a Raspberry Pi, with a known pack size for one of them:

```toml
targets = ["@nas.lan", "rack@pi.lan"]
charge-style = "lcd"
history = "48h"
recorder = "nas.lan"

[ups."rack@pi.lan"]
capacity = 864

[record]
keep = "30d"
listen = ["/run/upst/upst.sock", "0.0.0.0:3494"]
```

### Mistakes

upst refuses to start on a key it does not know or a value it cannot use, and says where:

```text
upst: /home/me/.config/upst/config.toml: TOML parse error at line 5, column 12
  |
5 | interval = "2x"
  |            ^^^^
unknown unit "x" in "2x" (use ms, s, m, h or d)
```

## Run the recorder

`upst record` polls like upst does, keeps a journal of every change per UPS, and serves the journals to upst, which replays the last `history` when it starts.
It holds no credentials and sends no commands.

### On NixOS

Add the flake input and import the module:

```nix
{
  inputs.upst.url = "github:Dolfost/upst";
  inputs.upst.inputs.nixpkgs.follows = "nixpkgs";
}
```

```nix
# In the system configuration, with inputs passed through specialArgs:
{ inputs, ... }:
{
  imports = [ inputs.upst.nixosModules.default ];

  services.upst = {
    enable = true;
    # Optional: /etc/upst/config.toml, with the keys above.
    settings = {
      charge-style = "lcd";               # everyone's default
      record = {
        targets = [ "ups@localhost" ];
        keep = "30d";
        listen = [ "0.0.0.0:3494" ];      # besides /run/upst/upst.sock
      };
    };
  };
}
```

| Option | Default | |
|---|---|---|
| `services.upst.enable` | `false` | Run the recorder and install upst. |
| `services.upst.package` | this flake's | The upst package. |
| `services.upst.settings` | `{ }` | The config file, as Nix values. It becomes `/etc/upst/config.toml`, which everyone's upst reads too. |
| `services.upst.settings.record.listen` | `[ ]` | Addresses to serve on besides `/run/upst/upst.sock`, which is always served. The firewall is left alone. |
| `services.upst.extraArgs` | `[ ]` | More arguments for `upst record`. |

This runs `upst.service` as a dynamic user in a hardened sandbox, keeps the journals in `/var/lib/upst`, serves them on `/run/upst/upst.sock`, puts `upst` on the `PATH`, and restarts the service when the settings change.
`settings.record.dir` is not allowed, since the sandbox can write only to `/var/lib/upst`.

### With systemd elsewhere

[contrib/upst.service](../contrib/upst.service) is the same service as a unit file:

```sh
sudo cp contrib/upst.service /etc/systemd/system/
sudo systemctl enable --now upst.service
```

It expects the binary in `/usr/local/bin`; adjust `ExecStart` if it is elsewhere.
It reads the `[record]` table of `/etc/upst/config.toml`, keeps the journals in `/var/lib/upst` and serves `/run/upst/upst.sock`.

### As a user service

[contrib/upst-user.service](../contrib/upst-user.service) records while you are logged in, or always with `loginctl enable-linger`:

```sh
cp contrib/upst-user.service ~/.config/systemd/user/upst.service
systemctl --user enable --now upst.service
```

It runs `~/.cargo/bin/upst record`, reads `[record]` from your config file, keeps the journals in `~/.local/state/upst`, and serves `$XDG_RUNTIME_DIR/upst/upst.sock`, where upst also looks.

### By hand

```sh
upst record                                        # every UPS on localhost
upst record ups@localhost @nas.lan --keep 30d
upst record --dir /srv/upst --listen /run/upst/upst.sock --listen 0.0.0.0:3494
```

The options are the keys of `[record]`: `-i`/`--interval`, `--keep`, `--dir`, `--listen` (repeatable) and `--timeout`, with targets as arguments.
They go after `record`.

### For other machines

Let the recorder listen on TCP as well, 3494 being the default port, and let that port through the firewall:

```toml
[record]
listen = ["/run/upst/upst.sock", "0.0.0.0:3494"]
```

Then on the other machine:

```sh
upst @nas.lan --recorder nas.lan
```

or `recorder = "nas.lan"` in its config file.
Anyone who can reach the port can read the history, just as anyone who can reach upsd can read the variables.

## Check that it works

```sh
upst --once                                                # one line per UPS
echo LIST | socat - UNIX-CONNECT:/run/upst/upst.sock      # what the recorder holds
journalctl -u upst.service                                 # where it writes and listens
```

The recorder's first log line says where the journals are, how long they are kept and where it listens.
In upst, an event "Recorded history loaded" appears at the start, and the Device card has a "Recorded" row with where the history came from and how far back it goes.

## Troubleshooting

**`connection refused`**: upsd is not running, or not listening on that address.
Try `upsc -l host` from the same machine, and see [Prepare upsd](#prepare-upsd).

**`not an upst recorder (it says ...)`**: `--recorder` points at something else, often upsd on 3493 instead of the recorder on 3494.

**No history although the recorder runs**: upst looks only for `/run/upst/upst.sock` and `$XDG_RUNTIME_DIR/upst/upst.sock`.
If `[record] listen` leaves both out, list the socket again or set `recorder`.
A UPS is matched to the journal with the same name and server, where the recorder's own `localhost` counts as the host it runs on; failing that, to the only journal with that UPS name.

**A setting has no effect**: the command line and the `UPST_*` variables win over the files, `~/.config/upst/config.toml` wins over `/etc/upst/config.toml`, and `UPST_CONFIG` or `--config` replaces both.
A system service reads only the system file.

**`record has to come first`**: options for the recorder go after the word `record`, as in `upst record --config FILE`.

**`listening on ...: another recorder is listening there`**: a second recorder was started on the same socket; stop one of them.
