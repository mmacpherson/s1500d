---
layout: default
title: s1500d — one-touch scanning on Linux without scanbd
---

# s1500d

**TL;DR:** s1500d is a tiny Rust daemon that monitors the Fujitsu ScanSnap S1500
via direct USB and runs your script when you press the scan button or insert
paper. One USB command per poll cycle, no SANE stack, no scanbd. Open the lid,
press the button, get a PDF.

## the problem

The ScanSnap S1500 is a fantastic color duplex document scanner. It launched in
2009 and has been out of production for years, but I've had mine since 2013 and
it still works great. We use it as the front door to our paperless household
([TODO][TODO]), paired with the excellent
[paperless-ngx](https://github.com/paperless-ngx/paperless-ngx) document
management system and some LLM postprocessing we do using
[Modal](https://modal.com) against the paperless-ngx API.

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

The usual answer is [scanbd](https://github.com/wilhelmbot/scanbd), a
general-purpose scanner button daemon. I've had mixed results with scanbd — over
different OSes and installations, I've usually gotten it to work, but I
struggled with it. Sometimes I couldn't get button-press detection to work, but
it could trigger on paper feed. Sometimes it would wait for nearly a minute
before actually beginning to scan the document. I'd followed the docs and the
ever-amazing [Arch
Wiki](https://wiki.archlinux.org/title/Scanner_Button_Daemon), and it's
ultimately probably user error. But now there's Claude Code, and I wanted to see
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
[protocol reference](protocol).

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
# Install via cargo
cargo install --path .

# Or via make (installs systemd unit, udev rules, config, etc.)
make release
sudo make install
```

See [INSTALL.md](https://github.com/mmacpherson/s1500d/blob/main/INSTALL.md) for the full details.

## quick start: seeing events

The simplest way to try s1500d is to just run it with no arguments. Open the scanner lid (which powers it on via USB), then:

```sh
s1500d
```

You'll see events logged to stderr as you interact with the scanner:

| Event | Meaning |
|-------|---------|
| `device-arrived` | Scanner lid opened (USB device appeared) |
| `device-left` | Scanner lid closed (USB device removed) |
| `paper-in` | Paper inserted into feeder |
| `paper-out` | Paper removed from feeder |
| `button-down` | Scan button pressed |
| `button-up` | Scan button released |

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
PDF using `scanimage` and `img2pdf`. Here's how it works:

```bash
#!/bin/bash
SCAN_DIR="${SCAN_DIR:-$HOME/Scans}"
EVENT="$1"
PROFILE="${2:-scan}"

case "$EVENT" in
    scan)
        mkdir -p "$SCAN_DIR"
        TIMESTAMP=$(date +%Y%m%d-%H%M%S)
        OUTFILE="$SCAN_DIR/${PROFILE}_${TIMESTAMP}.pdf"
        TMPDIR=$(mktemp -d)
        trap 'rm -rf "$TMPDIR"' EXIT

        logger -t s1500d "Scanning: profile=$PROFILE → $OUTFILE"

        scanimage \
            --device-name="fujitsu:ScanSnap S1500:*" \
            --source="ADF Duplex" \
            --mode=Color \
            --resolution=300 \
            --format=tiff \
            --batch="$TMPDIR/page_%04d.tiff" \
            --batch-count=0 \
            2>/dev/null

        PAGES=("$TMPDIR"/page_*.tiff)
        if [ ${#PAGES[@]} -eq 0 ] || [ ! -f "${PAGES[0]}" ]; then
            logger -t s1500d "No pages scanned"
            exit 1
        fi

        img2pdf "${PAGES[@]}" -o "$OUTFILE"
        logger -t s1500d "Saved $OUTFILE (${#PAGES[@]} pages)"
        ;;
    device-arrived)
        logger -t s1500d "Scanner ready"
        ;;
    device-left)
        logger -t s1500d "Scanner closed"
        ;;
esac
```

The pipeline is: `scanimage` pulls all pages from the ADF as TIFFs into a temp
directory, then `img2pdf` combines them into a single PDF. The profile name
(from gesture detection, described below) becomes the filename prefix, so you
can tell at a glance what kind of scan it was.

You'll need `sane` and `img2pdf` installed:

```sh
# Arch
pacman -S sane img2pdf

# Debian/Ubuntu
apt install sane-utils img2pdf

# Fedora
dnf install sane-backends img2pdf
```

## gesture mode

Running s1500d with a handler script directly is fine for simple setups, but
what if you want different scan settings depending on the situation — standard
vs. legal size, color vs. grayscale, simplex vs. duplex?

That's what config mode (`-c`) is for. Instead of passing raw button events to
your handler, s1500d counts rapid button presses within a timeout window and
maps the count to a named profile.

```toml
# config.toml
handler = "/path/to/your/handler.sh"
gesture_timeout_ms = 400
log_level = "info"

[profiles]
1 = "standard"
2 = "legal"
```

Press the button once, wait 400ms, and your handler gets called with `scan standard`. Press twice quickly and it gets `scan legal`. Unmapped press counts are logged and ignored.

Run it with:

```sh
s1500d -c config.toml
```

Your handler script just switches on the profile name — the scan-to-PDF handler
above already does this, using the profile as a filename prefix. You could go
further and change `scanimage` flags per profile (different resolution, page
size, simplex vs. duplex, etc.).

## running as a systemd service

For a proper always-on setup, you'll want a udev rule for non-root USB access and a systemd unit to keep the daemon running.

### udev rule

Install the udev rule so s1500d can access the scanner without root:

```sh
sudo cp contrib/99-scansnap.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
```

The rule itself is simple — it matches the S1500's USB vendor/product ID and opens permissions:

```
SUBSYSTEM=="usb", ATTR{idVendor}=="04c5", ATTR{idProduct}=="11a2", MODE="0666", TAG+="uaccess"
```

### systemd unit

```ini
[Unit]
Description=ScanSnap S1500 event daemon
Documentation=https://github.com/mmacpherson/s1500d
After=local-fs.target

[Service]
Type=simple
ExecStart=/usr/bin/s1500d -c /etc/s1500d/config.toml
Restart=always
RestartSec=5

# Hardening
NoNewPrivileges=true
ProtectHome=true

[Install]
WantedBy=multi-user.target
```

Copy it into place and enable:

```sh
sudo cp contrib/s1500d.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now s1500d
```

If you installed via the AUR package, the unit and udev rule are already in the right places — just enable the service.

## diagnosing hardware

If things aren't working, the `--doctor` flag runs an interactive hardware check that walks you through each sensor:

```sh
s1500d --doctor
```

It'll ask you to open the lid, insert paper, press the button, and so on — confirming that the daemon can see each event. Useful for verifying that USB permissions are set up correctly and the scanner is responding as expected.

## under the hood

The S1500 uses a vendor-specific USB protocol (class `FF:FF:FF`) with SCSI commands wrapped in a 31-byte Fujitsu envelope. The daemon sends a `GET_HW_STATUS` command (SCSI opcode `0xC2`) every 100ms and decodes the 12-byte response to detect button presses and paper presence. State transitions are edge-triggered — the handler only fires when something changes.

Door state isn't in the status response at all. Opening the ADF lid powers the scanner on (USB enumeration), closing it powers off (USB disconnect). So the daemon has two loops: an outer one watching for USB connect/disconnect, and an inner one polling `GET_HW_STATUS` while the device is present.

The protocol was reverse-engineered from USB captures and the SANE `fujitsu` backend source, then empirically verified with a physical scanner. The full details — including a SANE bit-map discrepancy I found during testing — are in the [protocol reference](protocol).

## references & prior art

s1500d exists because other people documented their ScanSnap-on-Linux setups and I could build on their work. These are the posts that informed the project:

- [Virantha Ekanayake — One-Touch Scan-To-PDF With ScanSnap S1500 on Linux](https://virantha.com/2014/03/17/one-touch-scanning-with-fujitsu-scansnap-s1500-on-linux/) (2014) — the original scanbd + S1500 walkthrough
- [Kevin Liu — Automatic Scanning on Linux with the ScanSnap S500M](https://kliu.io/post/linux-scansnap-s500m/) (2019) — scanbd setup for a different ScanSnap model
- [Neil Brown — Scanning to Debian 12 with ix500](https://neilzone.co.uk/2024/01/scanning-to-debian-12-with-a-scansnap-ix500) (2024) — scanbd on Debian with the ix500
- [J.B. Rainsberger — Use Your ScanSnap Scanner with Linux](https://blog.jbrains.ca/permalink/use-your-scansnap-scanner-with-linux) — general ScanSnap + Linux guidance

And the tools this project depends on or relates to:

- [SANE project](http://www.sane-project.org/) — Scanner Access Now Easy, the Linux scanning framework
- [scanbd](https://github.com/wilhelmbot/scanbd) — the general-purpose scanner button daemon
- [sane-backends fujitsu](https://gitlab.com/sane-project/backends/-/tree/master/backend) — the SANE backend that handles Fujitsu scanners (and where I found the USB protocol constants)
- [s1500d on GitHub](https://github.com/mmacpherson/s1500d) — source code, issues, and contributions welcome
