# CLAUDE.md — project guidance for Claude sessions

s1500d is a Linux event daemon for the Fujitsu ScanSnap S1500 scanner, written in Rust.

## Module layout

| File | Responsibility |
|------|---------------|
| `src/main.rs` | USB protocol (3-phase bulk transfer), state machine, event loop, handler dispatch |
| `src/config.rs` | TOML config parsing — `RawConfig` (serde) → `Config` (validated profile map) |
| `src/doctor.rs` | Interactive `--doctor` hardware check (walks user through each sensor) |

## Build and test

```sh
cargo build                # debug
cargo build --release      # release (stripped, LTO)
cargo test                 # unit tests (no hardware needed)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Pre-commit hooks enforce fmt + clippy on every commit.

## Key architectural decisions

- **Direct USB** via `rusb` — no kernel scanner driver needed, just a udev rule for permissions.
- **3-phase protocol**: send command → read status → interpret bits. The 6-byte status response encodes button, paper, and device presence as individual bits.
- **Gesture state machine** (config mode): counts rapid button presses within `gesture_timeout_ms`, then maps the count to a named profile.
- Two operational modes: **handler mode** (raw events → handler script) and **config mode** (`-c` flag, gesture detection + profile dispatch).

## contrib/ layout

| File | Purpose |
|------|---------|
| `config.toml` | Example configuration file |
| `handler-example.sh` | Minimal handler template |
| `handler-scan-to-pdf.sh` | Practical handler: scanimage → img2pdf |
| `s1500d.service` | Systemd unit with security hardening |
| `99-scansnap.rules` | udev rule for non-root USB access |
