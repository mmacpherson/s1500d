# s1500d

[![CI](https://github.com/mmacpherson/s1500d/actions/workflows/ci.yml/badge.svg)](https://github.com/mmacpherson/s1500d/actions/workflows/ci.yml)
[![AUR](https://img.shields.io/aur/version/s1500d)](https://aur.archlinux.org/packages/s1500d)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-orange)](https://www.rust-lang.org)

A minimal Linux daemon that watches a Fujitsu ScanSnap S1500 over USB and runs a
script of your choice when its button or paper sensors change. It is the event
trigger in a one-touch scanning workflow; your handler (usually `scanimage` plus
whatever post-processing you want) performs the scan.

[Read the project write-up](https://mmacpherson.github.io/s1500d/) for the
motivation, protocol investigation, and a complete worked example.

## Is this for you?

s1500d is intentionally narrow. It is a good fit if all of these are true:

- You have a **Fujitsu ScanSnap S1500**.
- The scanner is connected to a **Linux** machine (often a headless server).
- You want the physical scan button or paper sensor to launch your own script.

You can confirm the exact model by running `lsusb` on the Linux machine; an
S1500 appears as `ID 04c5:11a2`.

s1500d itself does not acquire images or require SANE; it detects hardware
events and invokes your handler. The example scan-to-PDF handler uses SANE's
`scanimage`, but your handler can run any command you choose.

It is specific to this scanner model rather than a general-purpose tool like
scanbd. Other ScanSnap models and macOS/Windows are not currently supported.

## Features

- **Runs a handler script** on scanner events (button press, paper inserted/removed, lid open/close)
- **Gesture detection** — optional TOML config maps multi-press patterns to named profiles (single press = standard scan, double press = legal size, etc.)
- **USB release during handler execution** — the daemon releases the USB device before calling your handler, so `scanimage` and other SANE tools can claim the scanner
- **`--doctor` mode** — interactive hardware verification that walks through each sensor
- **Lid detection via USB presence** — opening the automatic document feed (ADF) lid powers the scanner on (USB enumeration), closing it powers off (USB disconnect), so no polling is needed for door state

## Quick evaluation

You can install and confirm event detection before writing a handler or
enabling a service.

### Arch Linux (AUR)

```sh
paru -S s1500d
```

For Debian/Ubuntu, Fedora, other distributions, or a full systemd deployment,
see [INSTALL.md](INSTALL.md).

GitHub releases provide x86-64 deb, rpm, Arch `.pkg.tar.zst`, and generic tar
packages. The packages deliberately install the service without enabling or
starting it.

Then open the scanner's ADF lid and check the exact USB ID:

```sh
lsusb -d 04c5:11a2
s1500d --doctor
```

`--doctor` interactively checks USB communication, paper detection, and the
scan button. To simply watch events without running anything:

```sh
s1500d
```

Stop any other scanner-button daemon first; only one process can own the USB
interface. Neither command changes your system configuration or starts scans.

## Install with a coding agent

If you use Codex, Claude Code, or another coding agent with web access, paste
this prompt into it:

```text
Help me evaluate and install https://github.com/mmacpherson/s1500d. First
confirm that this machine runs Linux and that my scanner is exactly a ScanSnap
S1500 (`04c5:11a2`). Read README.md and INSTALL.md, explain the changes you
propose, and get event monitoring working before configuring a scan handler
or enabling the systemd service. Ask before using sudo.
```

The repository also includes [AGENTS.md](AGENTS.md) with project-specific
guidance for agents that clone the source.

## Usage

```
s1500d                        Monitor and log events (no handler)
s1500d HANDLER                Run HANDLER on each event
s1500d -c CONFIG.toml         Gesture detection + profile dispatch
s1500d --doctor               Interactive hardware verification
```

### Supported actions and gestures

This is the complete set of physical scanner actions s1500d recognizes:

| What you do | Raw handler mode | Config mode |
|-------------|------------------|-------------|
| Open the ADF lid | `device-arrived` | `device-arrived` |
| Close the ADF lid | `device-left` | `device-left` |
| Insert paper | `paper-in` | `paper-in` |
| Remove paper | `paper-out` | `paper-out` |
| Press the scan button | `button-down` | Starts or continues a multi-press gesture |
| Release the scan button | `button-up` | Completes one press and starts the gesture timeout |

In raw mode, the handler receives the event name as `$1`. In config mode,
button-down and button-up are not sent to the handler. Instead, one or more
complete presses followed by the timeout produce `scan <profile>` when that
press count appears in `[profiles]`. Any positive press count can be mapped;
unmapped counts are logged and ignored.

The complete button-gesture behavior in config mode is:

| What you do | What s1500d does |
|-------------|-----------------|
| Single press | Dispatches the profile mapped to `1` after the timeout |
| Double press | Dispatches the profile mapped to `2` after the timeout |
| Triple press | Dispatches the profile mapped to `3` after the timeout |
| Any higher configured number of presses | Dispatches the profile mapped to that number |

For a multi-press gesture, each next press must begin before the timeout after
the previous release expires. How long you hold the button does not affect the
gesture.

Set `log_level = "debug"` in your config file for verbose output. The `RUST_LOG` environment variable overrides config if set.

## Configuration

With `-c`, s1500d uses a TOML file to map button press counts to named profiles:

```toml
handler = "/path/to/your/handler.sh"
gesture_timeout_ms = 600
log_level = "info"

[profiles]
1 = "standard"
2 = "legal"
```

When you press the scan button once, the daemon waits `gesture_timeout_ms` for additional presses. If none come, it calls `handler.sh scan standard`. Two presses within the window call `handler.sh scan legal`. Unmapped press counts are logged and ignored.

Profile names such as `standard` and `legal` are arbitrary labels, not built-in
scan presets. Your handler decides what each one means: it can vary simplex or
duplex scanning, color mode, resolution, page size, output format or
destination, post-processing, OCR, upload, or anything else available to the
commands it runs. A handler is not limited to `scanimage`; it can run any
command you choose.

`log_level` accepts standard values: `error`, `warn`, `info`, `debug`, `trace`. The `RUST_LOG` environment variable overrides this setting if set.

See [`contrib/config.toml`](contrib/config.toml) for a full example and [`contrib/handler-example.sh`](contrib/handler-example.sh) for a handler template. For a practical scan-to-PDF workflow, see [`contrib/handler-scan-to-pdf.sh`](contrib/handler-scan-to-pdf.sh).

## How it works

The S1500 uses a vendor-specific USB protocol (class `FF:FF:FF`) with SCSI commands wrapped in a 31-byte Fujitsu envelope. The daemon sends a single `GET_HW_STATUS` command (SCSI opcode `0xC2`) every 100ms and decodes the 12-byte response to detect button presses and paper presence. State transitions are edge-triggered — the handler fires only when something changes.

The protocol was reverse-engineered from USB captures and the SANE `fujitsu` backend source code, then empirically verified with a physical scanner using the included [`docs/explore.py`](docs/explore.py) diagnostic tool.

See [`docs/protocol.md`](docs/protocol.md) for the full protocol reference.

## How this compares to [scanbd](https://gitlab.com/sane-project/frontend/scanbd)

**scanbd** is a general-purpose scanner button daemon. It loads the full SANE stack, opens a connection to the backend, and polls using SANE's option-reading API. For the S1500, this means:

- SANE opens the device, sends ~25 SCSI commands for initialization/capability queries
- Each poll cycle goes through the SANE abstraction layer
- scanbd must coordinate with `scanbm` to release/reacquire the SANE connection when scanning

**s1500d** bypasses SANE entirely:

- Opens the USB device directly via libusb
- Sends one 31-byte command, reads 12 bytes — no initialization sequence
- Releases the raw USB handle before calling your handler, so scanimage/SANE can claim the device cleanly

The tradeoff: s1500d only works with the ScanSnap S1500 (and potentially other ScanSnap models with compatible protocols). scanbd works with any SANE-supported scanner.

## Deployment

The repo includes systemd and udev files in [`contrib/`](contrib/):

- **`s1500d.service`** — systemd unit with security hardening
- **`70-s1500d.rules`** — udev rule for non-root USB access
- **`config.toml`** — example configuration
- **`handler-example.sh`** — example handler script
- **`handler-scan-to-pdf.sh`** — scan-to-PDF handler using `scanimage` + `img2pdf`

The packaged service runs as the dedicated `s1500d` user and sets `SCAN_DIR` to
`/var/lib/s1500d/scans`. Package installation does not enable or start it; first
follow [INSTALL.md](INSTALL.md), including the physical-device checks.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option. This is the standard dual-license convention used across the Rust ecosystem (rustc, serde, tokio, etc.).
