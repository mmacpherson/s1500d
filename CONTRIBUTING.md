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

There are no automated tests yet — the daemon requires physical USB hardware.

The best way to verify changes is `--doctor` mode, which walks through each sensor interactively:

```sh
cargo run -- --doctor
```

You can also run in log-only mode to watch events in real time:

```sh
RUST_LOG=debug cargo run
```

## Code style

- Run `cargo fmt` before committing (enforced by pre-commit hooks)
- Run `cargo clippy --all-targets -- -D warnings` to catch lint issues
- Keep the codebase minimal — s1500d is intentionally small

## Documenting new ScanSnap models

If you have a different ScanSnap model and want to map its hardware status bits:

1. Find your scanner's VID:PID with `lsusb`
2. Run the Python diagnostic tool:
   ```sh
   python3 docs/explore.py --discover
   ```
   This walks you through pressing the button, inserting paper, etc. and identifies which bits change.
3. Document your findings in a new section of `docs/protocol.md`
4. Open a PR with the new mapping — even partial data is valuable

See `docs/protocol.md` for details on the USB protocol and how to interpret the raw responses.

## License

By contributing, you agree that your contributions will be licensed under the same dual MIT/Apache-2.0 license as the project.
