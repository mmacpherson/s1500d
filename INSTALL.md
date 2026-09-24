# Installation

s1500d supports Linux and the Fujitsu ScanSnap S1500 with USB ID `04c5:11a2`.
It detects hardware events and calls a script; it does not scan documents by
itself. Before installing, confirm the scanner model and decide what script
should receive its events.

## Arch Linux (AUR)

```sh
paru -S s1500d
```

This installs the binary, systemd unit, udev rules, and example config/handler.
The example handler only writes events to the journal; it is safe for evaluating
the daemon but does not produce scans.

## GitHub release packages

GitHub releases include x86-64 deb, rpm, Arch `.pkg.tar.zst`, and generic tar
packages plus SHA-256 checksums. The native packages install the binary, systemd
unit, udev rule, dedicated service account definition, example config, and
handlers. They do not enable or start the service.

Use the deb on Debian/Ubuntu, the rpm on Fedora, and the `.pkg.tar.zst` with
`pacman -U` on Arch Linux. Arch users who prefer to build locally can inspect
the repository's [`PKGBUILD`](PKGBUILD) or use the AUR. The generic tar archive
is for inspecting or manually installing the same file layout on other Linux
distributions; it does not run package lifecycle scripts.

After downloading the files for a release, verify and install the one for your
distribution:

```sh
sha256sum --check SHA256SUMS

# Debian/Ubuntu
sudo apt install ./s1500d_*_amd64.deb

# Fedora
sudo dnf install ./s1500d-*.x86_64.rpm

# Arch Linux
sudo pacman -U ./s1500d-*-x86_64.pkg.tar.zst
```

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
# Get the source
git clone https://github.com/mmacpherson/s1500d.git
cd s1500d

# Install only the binary in your Cargo bin directory
cargo install --path .

# Or install the binary plus systemd, udev, config, and handler files
make release
sudo make install
sudo systemd-sysusers s1500d.conf
sudo udevadm control --reload-rules
sudo systemctl daemon-reload
```

See the [Makefile](Makefile) for configurable `PREFIX`, `DESTDIR`, `SYSCONFDIR`, and other variables.

## Verify the hardware

Open the ADF lid, then confirm the scanner and run the interactive check:

```sh
lsusb -d 04c5:11a2
s1500d --doctor
```

After installing a native package, also run the check as the same dedicated
account used by the service:

```sh
sudo -u s1500d s1500d --doctor
```

That command is the meaningful permission check on a headless machine. The
udev `uaccess` tag may separately allow the active desktop user to run the
unprivileged command.

Earlier versions installed the rule as `99-scansnap.rules`, too late in udev's
order for the desktop `uaccess` grant to apply. Packages and `make install`
remove that file; if you copied it to `/etc/udev/rules.d/` by hand, delete it
there and install `contrib/70-s1500d.rules` instead.

If `lsusb` sees the scanner but `--doctor` cannot open it, reload the installed
udev rule, close and reopen the lid, and try again:

```sh
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Stop scanbd or any other process that may own the scanner while testing. Running
`s1500d` with no arguments is a second, non-interactive check: it logs events
but does not call a handler.

## Configure what happens

There are two ways to run a handler:

```sh
# Raw events such as button-down and paper-in
s1500d /path/to/handler.sh

# Button gestures mapped to profiles in a TOML file
s1500d -c /path/to/config.toml
```

Start with [`contrib/config.toml`](contrib/config.toml) and
[`contrib/handler-example.sh`](contrib/handler-example.sh). The example handler
only logs. [`contrib/handler-scan-to-pdf.sh`](contrib/handler-scan-to-pdf.sh) is
a starting point for a real scan workflow and additionally requires SANE and
`img2pdf`:

```sh
# Arch Linux
pacman -S sane img2pdf

# Debian/Ubuntu
apt install sane-utils img2pdf

# Fedora
dnf install sane-backends img2pdf
```

Run and debug your chosen handler in the foreground before enabling it as a
service. The packaged service runs as the dedicated `s1500d` user, sets
`SCAN_DIR=/var/lib/s1500d/scans`, and uses systemd's `StateDirectory` support to
make `/var/lib/s1500d` writable by that account. It also sets `ProtectHome=true`,
so it cannot write to a user's `$HOME/Scans` without a systemd override.

The PDF handler automatically selects exactly one ScanSnap S1500 from
`scanimage -L` when `SCAN_DEVICE` is unset. With no matching device, multiple
matches, or failed discovery, it stops before scanning and reports the list
and setup instructions. For a single-scanner setup:

```sh
SCAN_DIR="$HOME/Scans" ./contrib/handler-scan-to-pdf.sh scan standard
```

To choose explicitly, set `SCAN_DEVICE` to the exact reported name, including
the serial number (not a wildcard). This skips discovery entirely:

```sh
SCAN_DEVICE='fujitsu:ScanSnap S1500:YOUR_SERIAL' SCAN_DIR="$HOME/Scans" \
    ./contrib/handler-scan-to-pdf.sh scan standard
```

Discovery runs before every scan. If `SANE_CONFIG_DIR` is unset, the handler
looks for readable `fujitsu.conf` in the current directory, `/etc/sane.d`, then
`/usr/local/etc/sane.d`. It copies that file into a private temporary directory
with only the Fujitsu backend enabled, used solely for discovery and then
removed. Acquisition retains the original SANE configuration.

An explicitly set `SANE_CONFIG_DIR` (including an empty value or search list)
bypasses this optimization and is passed through unchanged. If configuration
cannot be found/copied or the fast lookup fails or returns no devices, the
handler falls back to full discovery. A lookup returning only diagnostics or
other scanner models also falls back; it must list an S1500 to be usable.
Probing all enabled backends can add
several seconds before acquisition starts. For regular button-driven scanning, set
`SCAN_DEVICE` to the exact name logged by a successful detection to avoid that
delay. Detection does not save the name between invocations.
An off or unplugged scanner can produce an empty fast lookup, so such attempts
still pay the full discovery delay before failing. Pinning `SCAN_DEVICE` also
skips discovery in this case.

For service use, leave `SCAN_DEVICE` unset for auto-detection, or set it in the handler or in a systemd
drop-in with `[Service]` and
`Environment="SCAN_DEVICE=fujitsu:ScanSnap S1500:YOUR_SERIAL"`.
An explicitly empty value is an error; use `unset SCAN_DEVICE` to restore
auto-detection. Discovery runs as the handler's account and needs USB access.
The PDF handler creates scan directories with mode 0750 and completed PDFs
with mode 0640, owned by the invoking account/group. It preserves existing
directory permissions. Under the packaged service these files belong to
`s1500d:s1500d`; verify reader access as described below.

Failed attempts return nonzero and retain pages in private (0700)
`$SCAN_DIR/.s1500d-*` directories. Their paths appear on stderr and in the log.
An administrator or the service account can inspect and recover them; the last
TIFF may be incomplete after an acquisition error. Failed attempts are never
automatically deleted when they contain files. Empty working directories are
removed when no pages were acquired. Existing output filenames cause failure and retention,
not overwrite. The destination filesystem must support hard links, used to
publish the completed PDF atomically. See the [worked example](docs/index.md#scan-to-pdf).
If an importer or sync tool watches `SCAN_DIR` recursively, exclude `.s1500d-*`
directories or use a separate unwatched scan directory: recovery files and the
staged PDF are not ready for consumption.

Before enabling the service, check the config and handler without the scanner,
as the account the service runs as:

```sh
sudo -u s1500d s1500d --check-config /etc/s1500d/config.toml
```

This checks that the account can execute the handler. It cannot reproduce the
service's systemd sandbox (for example `ProtectHome=true`), so still test the
handler in the foreground as described above.

If `--doctor` or the service log reports that the scanner is in use, stop the
other program (scanbd, saned, another s1500d, or a running scan). If it
reports permission denied, check the udev rule and the account running s1500d.

## Enable the service

After `/etc/s1500d/config.toml` points to a tested handler:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now s1500d
systemctl status s1500d
journalctl -u s1500d -f
```

Review the unit, handler, output directory, and USB permissions for your system
before treating it as a production setup. In particular, confirm how the user
or process that consumes completed scans will read `/var/lib/s1500d/scans`. One
option is to add that user to the `s1500d` group (replace `USERNAME`, then log in
again):

```sh
sudo usermod --append --groups s1500d USERNAME
```

Package construction and service-file checks can be completed without a
scanner. USB permission, physical events, the USB-to-SANE handoff, scan output,
and disconnect recovery must be tested on the real device.
