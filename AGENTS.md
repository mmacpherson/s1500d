# Agent guidance for s1500d

s1500d is a Linux event daemon for the Fujitsu ScanSnap S1500 scanner. It talks
directly to USB to detect hardware events, releases the device, and invokes a
user-supplied handler. It does not scan documents itself.

## Repository map

| Path | Responsibility |
|------|----------------|
| `src/main.rs` | USB protocol, state machine, event loop, handler dispatch |
| `src/config.rs` | TOML parsing and validation |
| `src/doctor.rs` | Interactive physical-hardware check |
| `src/sim.rs` | Test-only scripted scanner that drives the real event loop |
| `tests/` | Stub-based tests for the example PDF handler |
| `contrib/` | Example handlers, config, udev rule, and systemd unit |
| `packaging/` | deb/rpm/tar/Arch release packaging |
| `docs/protocol.md` | Reverse-engineered USB protocol reference |

## Validate software-only changes

Run all of these before presenting a change:

```sh
cargo fmt --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
shellcheck contrib/*.sh packaging/*.sh
python3 -m unittest discover -s tests
```

The Rust tests do not require a scanner. Do not claim hardware validation unless
someone ran `s1500d --doctor` and the relevant end-to-end handler workflow on a
physical S1500.

Keep software and hardware validation separate. Software checks and package
inspection can be completed without a scanner; udev access, USB/SANE handoff,
physical events, output ownership, and recovery require the physical device.
Release packages must install the unit without enabling or starting it.

## Installation work

Before changing a user's machine:

1. Confirm Linux and the exact USB ID `04c5:11a2`.
2. Read `README.md` and `INSTALL.md`.
3. Explain commands that need root and ask before running them.
4. Stop competing scanner daemons, then get `s1500d --doctor` or log-only mode
   working before configuring a handler.
5. Test a handler in the foreground before enabling the systemd service.

Keep the defaults and examples in `src/config.rs`, `contrib/config.toml`,
`README.md`, and `docs/index.md` synchronized. Treat the shell handlers as
templates: output paths, ownership, SANE device names, and systemd sandboxing
must be checked for the target machine.
