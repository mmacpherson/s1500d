# s1500d

A minimal Rust daemon that monitors the Fujitsu ScanSnap S1500 scanner via direct USB, replacing [scanbd](https://github.com/wilhelmbot/scanbd) for button/paper detection. Where scanbd opens the full SANE stack and sends 25 SCSI commands per poll cycle, s1500d sends a single 31-byte USB command and reads 12 bytes — using a protocol reverse-engineered from USB captures and SANE source analysis.

## Features

- **Three operating modes:**
  - **Log-only** — monitor and log scanner events (button, paper, lid) with no handler
  - **Legacy** — run a handler script on each raw event (like scanbd)
  - **Config** — gesture detection with press-count profiles dispatched via TOML config
- **`--doctor` mode** — interactive hardware verification that walks through each sensor
- **USB release during handler execution** — the daemon releases the USB device before calling your handler, so `scanimage` and other SANE tools can claim the scanner
- **Lid detection via USB presence** — opening the ADF lid powers the scanner on (USB enumeration), closing it powers off (USB disconnect), so no polling is needed for door state

## Requirements

- **libusb**
  - Arch/CachyOS: `pacman -S libusb`
  - Debian/Ubuntu: `apt install libusb-1.0-0-dev`
  - Fedora: `dnf install libusb1-devel`
- **Rust toolchain** (for building from source) — install via [rustup](https://rustup.rs/)

## Installation

### From source

```sh
cargo install --path .
```

### Arch Linux (AUR)

```sh
# With an AUR helper:
paru -S s1500d

# Or manually:
git clone https://aur.archlinux.org/s1500d.git
cd s1500d && makepkg -si
```

### make install

```sh
make release
sudo make install
```

See the [Makefile](Makefile) for configurable `PREFIX`, `DESTDIR`, `SYSCONFDIR`, and other variables.

## Usage

```
s1500d                        Monitor and log events (no handler)
s1500d HANDLER                Legacy: run HANDLER on each raw event
s1500d -c CONFIG.toml         Config: gesture detection + profiles
s1500d --doctor               Interactive hardware verification
```

### Legacy mode

The handler script receives the event name as `$1`:

| Event | Meaning |
|-------|---------|
| `device-arrived` | Scanner lid opened (USB device appeared) |
| `device-left` | Scanner lid closed (USB device removed) |
| `paper-in` | Paper inserted into feeder |
| `paper-out` | Paper removed from feeder |
| `button-down` | Scan button pressed |
| `button-up` | Scan button released |

### Config mode

The handler receives structured arguments:

| Arguments | Meaning |
|-----------|---------|
| `scan <profile>` | Gesture completed — press count mapped to a profile name |
| `paper-in` | Paper inserted |
| `paper-out` | Paper removed |
| `device-arrived` | Scanner appeared |
| `device-left` | Scanner removed |

Set `RUST_LOG=debug` for verbose output.

## Configuration

Config mode uses a TOML file to map button press counts to named profiles:

```toml
handler = "/path/to/your/handler.sh"
gesture_timeout_ms = 400

[profiles]
1 = "standard"
2 = "legal"
```

When you press the scan button once, the daemon waits `gesture_timeout_ms` for additional presses. If none come, it calls `handler.sh scan standard`. Two presses within the window calls `handler.sh scan legal`. Unmapped press counts are logged and ignored.

See [`contrib/config.toml`](contrib/config.toml) for a full example and [`contrib/handler-example.sh`](contrib/handler-example.sh) for a handler template.

## How it works

The S1500 uses a vendor-specific USB protocol (class `FF:FF:FF`) with SCSI commands wrapped in a 31-byte Fujitsu envelope. The daemon sends a single `GET_HW_STATUS` command (SCSI opcode `0xC2`) every 100ms and decodes the 12-byte response to detect button presses and paper presence. State transitions are edge-triggered — the handler fires only when something changes.

The protocol was reverse-engineered from USB captures and the SANE `fujitsu` backend source code, then empirically verified with a physical scanner using the included [`docs/explore.py`](docs/explore.py) diagnostic tool.

See [`docs/protocol.md`](docs/protocol.md) for the full protocol reference.

## How this compares to scanbd

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
- **`99-scansnap.rules`** — udev rule for non-root USB access
- **`config.toml`** — example configuration
- **`handler-example.sh`** — example handler script

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option. This is the standard dual-license convention used across the Rust ecosystem (rustc, serde, tokio, etc.).
