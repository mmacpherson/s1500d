# Contributing to s1500d

## Development setup

1. Install Rust via [rustup](https://rustup.rs/)
2. Install libusb development headers:
   - Arch/CachyOS: `pacman -S libusb`
   - Debian/Ubuntu: `apt install libusb-1.0-0-dev`
3. Install [pre-commit](https://pre-commit.com/) and activate hooks:
   ```sh
   pre-commit install
   ```

## Building

```sh
cargo build            # debug build
cargo build --release  # release build (stripped, LTO)
```

## Testing

Run the test suites with:

```sh
cargo test --all-targets --locked
python3 -m unittest discover -s tests
```

The Rust tests cover configuration validation, USB transaction checks, and the
real event loop driven by a scripted scanner (`src/sim.rs`), including handler
handoff, reconnects, and gestures. The Python tests run
`contrib/handler-scan-to-pdf.sh` against stub `scanimage`/`img2pdf` commands. No
hardware is required.

For **physical hardware**, `--doctor` checks that each sensor event is seen
(a real scan workflow still needs testing with your handler):

```sh
cargo run -- --doctor
```

You can also run in log-only mode to watch events in real time:

```sh
RUST_LOG=debug cargo run
```

## Module layout

| File | Responsibility |
|------|---------------|
| `src/main.rs` | USB protocol, state machine, event loop, handler dispatch |
| `src/config.rs` | TOML config parsing and validation |
| `src/doctor.rs` | Interactive `--doctor` hardware check |
| `src/sim.rs`, `src/sim/` | Scripted scanner for event-loop tests (test-only) |
| `tests/` | Tests for the example PDF handler |

## Code style

- Run `cargo fmt` before committing (enforced by pre-commit hooks)
- Run `cargo clippy --all-targets -- -D warnings` to catch lint issues
- Shell scripts are checked with ShellCheck (enforced by pre-commit hooks)
- Keep the codebase minimal — s1500d is intentionally small

## Documenting new ScanSnap models

If you have a different ScanSnap model and want to map its hardware status bits:

1. Find your scanner's VID:PID with `lsusb`
2. Set `VID` and `PID` near the top of `docs/explore.py` to your scanner's ID, then run the Python diagnostic tool:
   ```sh
   python3 docs/explore.py --discover
   ```
   This walks you through pressing the button, inserting paper, etc. and identifies which bits change.
3. Document your findings in a new section of `docs/protocol.md`
4. Open a PR with the new mapping — even partial data is valuable

See `docs/protocol.md` for details on the USB protocol and how to interpret the raw responses.

## License

By contributing, you agree that your contributions will be licensed under the same dual MIT/Apache-2.0 license as the project.
