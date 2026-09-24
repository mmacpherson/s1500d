---
layout: default
title: one-touch scanning on Linux without scanbd
---

*[source and installation](https://github.com/mmacpherson/s1500d)*

**TL;DR:** s1500d is a tiny Rust daemon that monitors the Fujitsu ScanSnap S1500
via direct USB and runs your script when you press the scan button or insert
paper. One USB command per poll cycle, no SANE stack, no scanbd. With a scan
handler configured: open the lid, press the button, get a PDF.

## should you use this?

s1500d is deliberately for one setup: a **Fujitsu ScanSnap S1500** attached to a
**Linux** machine. You can confirm the exact model with `lsusb`; an S1500 appears
as `ID 04c5:11a2`. It is useful when you want the physical button or paper
sensor to run a script without leaving a GUI open.

s1500d itself does not acquire images or require SANE; it detects hardware
events and invokes your handler. The example scan-to-PDF handler uses SANE's
`scanimage`, but your handler can run any command you choose.

It is specific to this scanner model rather than a general-purpose button
daemon. Other ScanSnap models and macOS/Windows are not currently supported. If
that still sounds like your setup, the quickest safe evaluation is:

```sh
# Arch Linux
paru -S s1500d

# Open the scanner lid, then:
lsusb -d 04c5:11a2
s1500d --doctor
s1500d  # watch events; does not start a scan
```

For other Linux distributions and service setup, use the full
[installation guide](https://github.com/mmacpherson/s1500d/blob/main/INSTALL.md).

If you prefer an assisted install, paste this into Codex, Claude Code, or
another coding agent with web access:

```text
Help me evaluate and install https://github.com/mmacpherson/s1500d. First
confirm that this machine runs Linux and that my scanner is exactly a ScanSnap
S1500 (`04c5:11a2`). Read README.md and INSTALL.md, explain the changes you
propose, and get event monitoring working before configuring a scan handler
or enabling the systemd service. Ask before using sudo.
```

## the problem

The ScanSnap S1500 is a fantastic color duplex document scanner. It launched in
2009 and has been out of production for years, but I've had mine since 2013 and
it still works great. We use it as the front door to our paperless household
([a well-trodden path](https://toolsandtoys.net/guides/the-tools-and-toys-paperless-guide/)),
paired with [paperless-ngx](https://github.com/paperless-ngx/paperless-ngx)
and some LLM postprocessing via [Modal](https://modal.com), all running on an
Arch Linux homelab server.

On Mac and Windows, the software situation is bleak — Fujitsu's current
[ScanSnap Home](https://www.pfu.ricoh.com/global/scanners/scansnap/dl/) doesn't
support it at all, and they
[discontinued ScanSnap Manager](https://talk.tidbits.com/t/fujitsu-discontinues-scansnap-software-support-again/29358)
(the legacy software that did) in November 2024. You can still find old ScanSnap
Manager installers floating around the web, and apparently they still work, but
it's not a great long-term bet. On Linux, none of that matters — SANE's
`fujitsu` backend handles scanning just fine. The hard part is the "one-touch"
workflow: you want to press the physical scan button and have something happen
automatically, without a GUI open and waiting.

The usual answer is [scanbd](https://gitlab.com/sane-project/frontend/scanbd), a
general-purpose scanner button daemon. I've had mixed results with scanbd — over
different OSes and installations, I've usually gotten it to work, but I
struggled with it. Sometimes I couldn't get button-press detection to work, but
it could trigger on paper feed. Sometimes it would wait for nearly a minute
before actually beginning to scan the document. I'd followed the docs and the
ever-amazing [Arch
Wiki](https://wiki.archlinux.org/title/Scanner_Button_Daemon), and it's
ultimately probably user error. But now there's [Claude Code](https://docs.anthropic.com/en/docs/claude-code), and I wanted to see
if I could get something that works more consistently for me.

I asked Claude to help me figure out how the scanner was actually communicating
with the computer. It wrote a diagnostic script
([explore.py](https://github.com/mmacpherson/s1500d/blob/main/docs/explore.py))
that captured USB traffic using
[termshark](https://github.com/gcla/termshark) (a terminal UI for Wireshark),
then guided me through a protocol — insert paper, remove paper, press the
button, release the button — recording which bits changed at each step. Between
that and reading the SANE `fujitsu` backend source, we reverse-engineered the
USB protocol. It's actually pretty simple: one command (`GET_HW_STATUS`), 12
bytes of response, a few status bits to decode. Full details in the
[protocol reference](https://mmacpherson.github.io/s1500d/protocol.html).

scanbd takes a different approach — it loads the full SANE stack, opens a
connection to the backend, and polls by reading SANE options, about 25 SCSI
commands per cycle. It needs a `scanbm` proxy to coordinate device access
between polling and scanning. Those are reasonable decisions if you're building
something that supports every SANE-compatible scanner. But if you only need to
talk to one device, you can skip all of that and send the one command directly
via libusb.

That's what s1500d does. The tradeoff is clear: it only works with the ScanSnap
S1500 (and potentially other ScanSnap models with compatible protocols — I've
only tested the S1500). If you have an S1500 and want something minimal that
just works, read on. Or if you're interested in a kind of template for using a
coding agent to reverse-engineer a USB protocol and build a bespoke driver for
some other piece of hardware, this might be a useful case study.

## installation

### Arch Linux (AUR)

I published an [AUR package](https://aur.archlinux.org/packages/s1500d) for my
own convenience, but in case one of the other six people running an S1500 on
Arch Linux reads this:

```sh
paru -S s1500d
```

This installs the binary, systemd unit, udev rules, and example config/handler.

### from source

You'll need libusb and a Rust toolchain:

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
```

See [INSTALL.md](https://github.com/mmacpherson/s1500d/blob/main/INSTALL.md) for the full details.

## quick start: seeing events

The simplest way to try s1500d is to just run it with no arguments. Open the scanner lid (which powers it on via USB), then:

```sh
s1500d
```

You'll see events logged to stderr as you interact with the scanner.

### supported actions and gestures

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

To actually *do* something with these events, pass a handler script:

```sh
s1500d /path/to/handler.sh
```

The handler receives the event name as `$1`. Here's a minimal example:

```bash
#!/bin/bash
EVENT="$1"
PROFILE="${2:-}"

case "$EVENT" in
    scan)
        logger -t s1500d "Scan gesture: profile=$PROFILE"
        # Your scan logic here — scanimage is safe to call,
        # s1500d has released the USB device.
        ;;
    paper-in)
        logger -t s1500d "Paper detected"
        ;;
    button-down)
        logger -t s1500d "Scan button pressed (legacy mode)"
        ;;
    device-arrived)
        logger -t s1500d "Scanner lid opened"
        ;;
    device-left)
        logger -t s1500d "Scanner lid closed"
        ;;
    *)
        logger -t s1500d "Event: $EVENT"
        ;;
esac
```

One important detail: s1500d releases the USB device before calling your
handler. This means `scanimage` and other SANE tools can claim the scanner
cleanly — no fighting over the device handle.

## scan to PDF

The
[contrib/handler-scan-to-pdf.sh](https://github.com/mmacpherson/s1500d/blob/main/contrib/handler-scan-to-pdf.sh)
script is a practical handler that scans all pages in the ADF to a timestamped
PDF using `scanimage` and `img2pdf`. Use the maintained script linked above.
With `SCAN_DEVICE` unset, the handler runs `scanimage -L`, selects exactly one
ScanSnap S1500 and logs its exact name. No matches, multiple matches or a failed
lookup stop the attempt with the device list and instructions. Other scanner
models do not count as matches.

Discovery runs on every scan. With `SANE_CONFIG_DIR` unset, it looks for a
readable `fujitsu.conf` in the current directory, `/etc/sane.d`, then
`/usr/local/etc/sane.d`, and tries a private temporary Fujitsu-only configuration.
This configuration is removed afterwards and never changes acquisition settings.
Explicit `SANE_CONFIG_DIR` values, including empty values and search lists, use
full discovery unchanged. Missing configuration, preparation errors, failed lookups, or
fast lookups without an S1500 device entry also fall back to full discovery.
Diagnostic text alone does not count as a device. That can add several
seconds while enabled backends are probed. For regular use, set
`SCAN_DEVICE` to the exact name in the detection log to skip that delay;
the handler does not persist its selection between invocations.
An off or unplugged scanner can leave the fast lookup empty, triggering the
slower full lookup before failure. Pinning `SCAN_DEVICE` skips that lookup too.

For a single-scanner setup:

```sh
SCAN_DIR="$HOME/Scans" ./contrib/handler-scan-to-pdf.sh scan standard
```

An explicit `SCAN_DEVICE` skips lookup. Use the exact name (including serial
number); a literal `*` is not a device selector. For example:

```sh
scanimage -L
SCAN_DEVICE='fujitsu:ScanSnap S1500:YOUR_SERIAL' SCAN_DIR="$HOME/Scans" \
    ./contrib/handler-scan-to-pdf.sh scan standard
```

The handler acquires TIFF pages into a private directory
`$SCAN_DIR/.s1500d-XXXXXXXXXX`, then converts them to a staged PDF. It publishes
`standard_YYYYMMDD-HHMMSS.pdf` only after successful acquisition and conversion.
An existing destination is never overwritten: a same-second filename collision
fails and retains the new attempt for recovery. Successful attempts remove their
working files.

Acquisition errors (including a jam after some pages), conversion errors, and
publication failures return nonzero, retain recovery files, and print their
location to stderr and the system log. Scanner and converter diagnostics remain
visible. HUP/INT/TERM also retain the attempt; after an uncatchable termination,
look for the hidden `.s1500d-*` directories. Inspect partial TIFFs before using
them: the last page may be incomplete. Nothing automatically retries or deletes
failed attempts containing files. Attempts with no pages remove their working
directory only if it is empty, then report failure with the scanner exit code.
Recover or remove retained attempts manually after checking their contents;
a retained `scan.pdf` is not proof of a complete acquisition.

Files belong to the invoking account. New scan directories are mode 0750,
completed PDFs are 0640, and recovery directories are private (0700).
Existing scan-directory permissions are not changed. Under the packaged service,
the owner/group is `s1500d` and `SCAN_DIR=/var/lib/s1500d/scans`; readers need
appropriate group membership and directory access. Recovery requires the service
account or an administrator. Leave `SCAN_DEVICE` unset for auto-detection, or
set it in the handler or a systemd environment override. An explicitly empty
value is an error; `unset SCAN_DEVICE` restores auto-detection. Verify foreground
scanning and access before enabling
the service. This example requires a filesystem supporting hard links for atomic
publication; publication failure keeps the recovery files.

Keep `SCAN_DIR` outside recursively watched import or sync folders, or configure
those consumers to exclude `.s1500d-*` directories. These directories contain raw
pages and a PDF while it is still being written; only top-level published PDFs
are ready for consumption.

You'll need `sane` and `img2pdf` installed:

```sh
# Arch
pacman -S sane img2pdf

# Debian/Ubuntu
apt install sane-utils img2pdf

# Fedora
dnf install sane-backends img2pdf
```

## configuration

Running s1500d with `-c` enables config mode, which reads a TOML file:

```sh
s1500d -c config.toml
```

Here's a full example:

```toml
handler = "/path/to/your/handler.sh"
gesture_timeout_ms = 600
log_level = "info"

[profiles]
1 = "standard"
2 = "legal"
3 = "photo"
```

### config keys

| Key | Required | Default | Description |
|-----|----------|---------|-------------|
| `handler` | yes | — | Path to the script called on events |
| `gesture_timeout_ms` | no | `600` | How long to wait (in ms) for additional button presses before dispatching a gesture |
| `log_level` | no | `"info"` | Log verbosity: `error`, `warn`, `info`, `debug`, `trace`. The `RUST_LOG` environment variable overrides this if set. |
| `profiles` | no | (empty) | Map of press count → profile name (see below). Profile names are arbitrary labels — your handler script decides what they mean. |

### events in config mode

In config mode, your handler receives these events as `$1`:

| Event | `$2` | When it fires |
|-------|------|---------------|
| `scan` | profile name | Button gesture completed (press count mapped to a profile) |
| `paper-in` | — | Paper inserted into feeder |
| `paper-out` | — | Paper removed from feeder |
| `device-arrived` | — | Scanner lid opened (USB device appeared) |
| `device-left` | — | Scanner lid closed (USB device removed) |

### gesture detection

Instead of passing raw `button-down`/`button-up` events, config mode counts
complete button presses and maps the count to a named profile via the
`[profiles]` table. Each release starts the `gesture_timeout_ms` window; another
press inside it increments the count, while letting it expire dispatches the
gesture.

Press the button once, wait 600ms, and your handler gets called with
`scan standard`. Press twice quickly and it gets `scan legal`. Three times for
`scan photo`. Unmapped press counts are logged and ignored.

The config above maps three press counts, but s1500d does not hard-code a finite
set of single-, double-, or triple-press gestures. Any positive press count can
be mapped, and your handler can support as many profiles as you find practical.

Profile names such as `standard`, `legal`, and `photo` are arbitrary labels,
not built-in scan presets. Your handler decides what they mean. With
`scanimage`, a profile might select simplex or duplex scanning, color mode,
resolution, page dimensions, or output format; the handler can also choose a
destination, perform OCR or other post-processing, upload the result, send a
notification, or run something unrelated to scanning. Here are some natural
scan settings for reference:

For example, add this before the handler's acquisition step:

```bash
SCAN_OPTIONS=(--source="ADF Duplex" --mode=Color --resolution=300)
case "$PROFILE" in
    legal) SCAN_OPTIONS+=(--page-width=215.872 --page-height=355.6 -x 215.872 -y 355.6) ;;
    a4) SCAN_OPTIONS+=(--page-width=210 --page-height=297 -x 210 -y 297) ;;
    photo) SCAN_OPTIONS=(--source="ADF Front" --mode=Color --resolution=600) ;;
    standard-bw) SCAN_OPTIONS=(--source="ADF Duplex" --mode=Lineart --resolution=300) ;;
    standard-gray) SCAN_OPTIONS=(--source="ADF Duplex" --mode=Gray --resolution=300) ;;
esac
```

Replace the handler's fixed source, mode, and resolution arguments with
`"${SCAN_OPTIONS[@]}"`, keeping its exact device selection, batch path, and
acquisition/conversion failure handling. The profile remains the filename prefix;
use only letters, digits, underscores, and hyphens.

## running as a systemd service

The repository ships a udev rule and systemd unit as starting points for an
always-on setup. Review them for your machine rather than treating them as a
turnkey policy:

- The udev rule grants the dedicated `s1500d` account access to this USB device
  and uses `uaccess` for the active desktop user.
- The unit runs as `s1500d`, not root.
- `ProtectHome=true` prevents writes to users' homes; the unit sets `SCAN_DIR`
  to `/var/lib/s1500d/scans` instead.
- The default handler logs events but does not scan documents.

Test your handler in the foreground, edit `/etc/s1500d/config.toml`, and only
then enable the installed unit:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now s1500d
```

See the
[installation guide](https://github.com/mmacpherson/s1500d/blob/main/INSTALL.md)
for package and from-source paths.

## diagnosing hardware

If things aren't working, the `--doctor` flag runs an interactive hardware check that walks you through each sensor:

```sh
s1500d --doctor
```

It'll ask you to open the lid, insert paper, press the button, and so on — confirming that the daemon can see each event. Useful for verifying that USB permissions are set up correctly and the scanner is responding as expected.

## under the hood

The S1500 uses a vendor-specific USB protocol (class `FF:FF:FF`) with SCSI commands wrapped in a 31-byte Fujitsu envelope. The daemon sends a `GET_HW_STATUS` command (SCSI opcode `0xC2`) every 100ms and decodes the 12-byte response to detect button presses and paper presence. State transitions are edge-triggered — the handler only fires when something changes.

Door state isn't in the status response at all. Opening the ADF lid powers the scanner on (USB enumeration), closing it powers off (USB disconnect). So the daemon has two loops: an outer one watching for USB connect/disconnect, and an inner one polling `GET_HW_STATUS` while the device is present.

The protocol was reverse-engineered from USB captures and the SANE `fujitsu` backend source, then empirically verified with a physical scanner. The full details — including a SANE bit-map discrepancy I found during testing — are in the [protocol reference](https://mmacpherson.github.io/s1500d/protocol.html).

## references & prior art

s1500d exists because other people documented their ScanSnap-on-Linux setups and I could build on their work. These are the posts that informed the project:

- [Virantha Ekanayake — One-touch scan enabling in Ubuntu Linux for the Fujitsu ScanSnap S1500](https://virantha.com/2014/03/17/one-touch-scanning-with-fujitsu-scansnap-in-linux/) (2014) — the original scanbd + S1500 walkthrough
- [Kevin Liu — Fully Automatic Scanning with the ScanSnap S500M on Linux](https://kliu.io/post/automatic-scanning-with-scansnap-s500m/) (2019) — scanbd setup for a different ScanSnap model
- [Neil Brown — Scanning to Debian 12 with a Fujitsi ix500](https://neilzone.co.uk/2024/03/scanning-to-debian-12-with-a-fujitsi-ix500/) (2024) — scanbd on Debian with the ix500
- [J.B. Rainsberger — Use a Fujitsu ScanSnap Scanner With Linux](https://jb.rainsberger.ca/permalink/use-scansnap-with-linux) — general ScanSnap + Linux guidance

Going paperless at home:

- [Tools and Toys — Setting Up and Maintaining a Paperless Home and Office](https://toolsandtoys.net/guides/the-tools-and-toys-paperless-guide/) — the classic ScanSnap-centric paperless guide
- [DocumentSnap](https://www.documentsnap.com/) — Brooks Duncan's site dedicated to going paperless, including the [Unofficial ScanSnap Setup Guide](https://www.documentsnap.com/unofficial-scansnap-setup-guide-fourth-edition/)
- [Techno Tim — Self-Hosted Paperless-ngx + Local AI](https://technotim.com/posts/paperless-ngx-local-ai/) — modern Docker-based setup with local AI for OCR/classification
- [Akash Rajpurohit — Paperless-ngx: Self-hosted document management that actually makes sense](https://akashrajpurohit.com/blog/selfhost-paperless-ngx-for-document-management/)
- [Redeeming Productivity — The Ultimate Guide to Going Paperless at Home](https://redeemingproductivity.com/paperless/)

And the tools this project depends on or relates to:

- [SANE project](http://www.sane-project.org/) — Scanner Access Now Easy, the Linux scanning framework
- [scanbd](https://gitlab.com/sane-project/frontend/scanbd) — the general-purpose scanner button daemon
- [sane-backends fujitsu](https://gitlab.com/sane-project/backends/-/tree/master/backend) — the SANE backend that handles Fujitsu scanners (and where I found the USB protocol constants)
- [s1500d on GitHub](https://github.com/mmacpherson/s1500d) — source code, issues, and contributions welcome
