# Installation

## Arch Linux (AUR)

```sh
paru -S s1500d
```

This installs the binary, systemd unit, udev rules, and example config/handler.

## From source

Requires libusb and a Rust toolchain:

```sh
# Arch/CachyOS
pacman -S libusb

# Debian/Ubuntu
apt install libusb-1.0-0-dev

# Fedora
dnf install libusb1-devel
```

Then either:

```sh
# Install via cargo
cargo install --path .

# Or via make (installs systemd unit, udev rules, config, etc.)
make release
sudo make install
```

See the [Makefile](Makefile) for configurable `PREFIX`, `DESTDIR`, `SYSCONFDIR`, and other variables.
