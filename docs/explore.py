#!/usr/bin/env python3
"""
ScanSnap S1500 hardware status explorer.

Sends GET_HW_STATUS (0xC2) via the Fujitsu USB wrapper protocol
and decodes the 12-byte response into human-readable sensor state.

Protocol notes (from SANE fujitsu backend + USB capture analysis):
  - VID 0x04C5, PID 0x11A2
  - Vendor-specific USB class (FF:FF:FF), bulk-only endpoints
  - Fujitsu wraps SCSI CDBs in a 31-byte envelope:
      byte 0:    0x43 (USB_COMMAND_CODE)
      bytes 1-18: zeros (padding)
      bytes 19+:  SCSI CDB (up to 12 bytes)
  - Protocol is 3-phase: command → data → status
    (status phase returns 0x53 envelope for success)

GET_HW_STATUS response (12 bytes):
  byte 0-2:  reserved
  byte 3:    ADF status  (bit 7: hopper, inverted; bit 6: ADF open; others TBD)
  byte 4:    buttons     (bit 0: scan switch; bit 1: manual feed; bit 2: send SW)
  byte 5:    consumables (roller/pad wear alerts)
  byte 6:    error flags
  byte 7-11: reserved / error codes

Usage:
    sudo python3 explore.py                # monitor for state changes (~100ms poll)
    sudo python3 explore.py --once         # single read, full hex dump
    sudo python3 explore.py --raw          # continuous raw hex output
    sudo python3 explore.py --discover     # systematic bit discovery mode

Requires: pyusb (pip install pyusb)
Must stop scanner-watch.sh first:
    sudo kill $(pgrep -f scanner-watch)
"""

from __future__ import annotations

import sys
import time
from dataclasses import dataclass, field

import usb.core
import usb.util


# --- Device constants ---

VID = 0x04C5
PID = 0x11A2
EP_OUT = 0x02
EP_IN = 0x81

USB_CMD_CODE = 0x43
USB_CMD_LEN = 31
USB_CMD_OFFSET = 19  # SCSI CDB starts here


# --- Protocol helpers ---

def make_envelope(cdb: bytes) -> bytes:
    """Wrap a SCSI CDB in the Fujitsu 31-byte USB command envelope."""
    buf = bytearray(USB_CMD_LEN)
    buf[0] = USB_CMD_CODE
    buf[USB_CMD_OFFSET : USB_CMD_OFFSET + len(cdb)] = cdb
    return bytes(buf)


def hex_of(data: bytes) -> str:
    return " ".join(f"{b:02x}" for b in data)


def bits_of(b: int) -> str:
    return f"{b:08b}"


# SCSI CDB for GET_HW_STATUS: opcode 0xC2, alloc length 12 at bytes 7-8
GHS_CDB = bytes([0xC2, 0, 0, 0, 0, 0, 0, 0, 0x0C, 0])
GHS_CMD = make_envelope(GHS_CDB)

# SCSI CDB for TEST UNIT READY: all zeros (6-byte CDB)
TUR_CDB = bytes(6)
TUR_CMD = make_envelope(TUR_CDB)


# --- State decoding ---

@dataclass(frozen=True)
class HWStatus:
    """Decoded GET_HW_STATUS response. Immutable snapshot of scanner state."""

    raw: bytes

    # Byte 3 - ADF / paper
    hopper: bool      # paper in hopper (bit 7, inverted per SANE)
    adf_open: bool    # ADF door open (bit 6, tentative)
    paper_end: bool   # paper path clear / end (bit 5, tentative)

    # Byte 4 - buttons / switches
    scan_sw: bool     # scan button (bit 0)
    manual_feed: bool # manual feed (bit 1, tentative)
    send_sw: bool     # send/email button (bit 2, tentative)

    # Raw interesting bytes for discovery
    b3: int = 0
    b4: int = 0
    b5: int = 0
    b6: int = 0

    @classmethod
    def from_response(cls, data: bytes) -> HWStatus:
        raw = bytes(data[:12]) if len(data) >= 12 else bytes(data)
        b3 = raw[3] if len(raw) > 3 else 0
        b4 = raw[4] if len(raw) > 4 else 0
        b5 = raw[5] if len(raw) > 5 else 0
        b6 = raw[6] if len(raw) > 6 else 0

        return cls(
            raw=raw,
            hopper=not bool(b3 & 0x80),     # bit 7 inverted
            adf_open=bool(b3 & 0x40),       # bit 6
            paper_end=bool(b3 & 0x20),      # bit 5
            scan_sw=bool(b4 & 0x01),        # bit 0
            manual_feed=bool(b4 & 0x02),    # bit 1
            send_sw=bool(b4 & 0x04),        # bit 2
            b3=b3, b4=b4, b5=b5, b6=b6,
        )

    def diff(self, prev: HWStatus) -> list[str]:
        """Human-readable list of fields that changed from prev."""
        changes = []
        named_fields = [
            ("hopper", "paper in feeder"),
            ("adf_open", "ADF door open"),
            ("paper_end", "paper path end"),
            ("scan_sw", "scan button"),
            ("manual_feed", "manual feed"),
            ("send_sw", "send button"),
        ]
        for attr, label in named_fields:
            old, new = getattr(prev, attr), getattr(self, attr)
            if old != new:
                changes.append(f"  {label}: {old} → {new}")

        # Also flag any bit changes in raw bytes (catch unmapped bits)
        for i in range(len(self.raw)):
            old_b, new_b = prev.raw[i], self.raw[i]
            if old_b != new_b:
                changes.append(
                    f"  byte[{i}]: 0x{old_b:02x} ({bits_of(old_b)}) "
                    f"→ 0x{new_b:02x} ({bits_of(new_b)})"
                )
        return changes


# --- USB communication ---

def open_scanner() -> usb.core.Device:
    """Find, claim, and configure the ScanSnap S1500."""
    dev = usb.core.find(idVendor=VID, idProduct=PID)
    if dev is None:
        print("ScanSnap S1500 not found. Is the lid open?", file=sys.stderr)
        sys.exit(1)

    # Detach kernel driver (SANE / libusb may have it)
    try:
        if dev.is_kernel_driver_active(0):
            dev.detach_kernel_driver(0)
            print("(detached kernel driver)")
    except (usb.core.USBError, NotImplementedError):
        pass

    # Reset device to clear any stale state from previous users
    # (e.g., scanner-watch.sh killed mid-transaction)
    print("(resetting USB device...)")
    dev.reset()
    time.sleep(0.5)

    # Re-find after reset (device handle may be invalidated)
    dev = usb.core.find(idVendor=VID, idProduct=PID)
    if dev is None:
        print("Device disappeared after reset!", file=sys.stderr)
        sys.exit(1)

    try:
        if dev.is_kernel_driver_active(0):
            dev.detach_kernel_driver(0)
    except (usb.core.USBError, NotImplementedError):
        pass

    dev.set_configuration()
    usb.util.claim_interface(dev, 0)

    # Clear any stalled endpoints
    try:
        usb.control.clear_feature(dev, usb.control.ENDPOINT_HALT, EP_OUT)
        usb.control.clear_feature(dev, usb.control.ENDPOINT_HALT, EP_IN)
    except usb.core.USBError:
        pass  # Not stalled, that's fine

    return dev


def usb_transact(dev, cmd: bytes, expect_data: bool = True) -> tuple[bytes, bytes | None]:
    """
    Execute one Fujitsu USB command transaction.

    3-phase protocol:
      1. Write command envelope → EP_OUT
      2. Read data response → EP_IN  (if expect_data)
      3. Read status (0x53) → EP_IN  (always, to keep pipe clean)

    Returns (data_or_status, status_or_None).
    """
    dev.write(EP_OUT, cmd, timeout=1000)
    resp1 = bytes(dev.read(EP_IN, 512, timeout=1000))

    if not expect_data:
        return resp1, None

    # Try to read the status phase
    try:
        resp2 = bytes(dev.read(EP_IN, 512, timeout=200))
    except usb.core.USBTimeoutError:
        resp2 = None

    return resp1, resp2


def test_unit_ready(dev) -> bool:
    """Send TEST UNIT READY, return True if device responds with 0x53."""
    resp, _ = usb_transact(dev, TUR_CMD, expect_data=False)
    return len(resp) > 0 and resp[0] == 0x53


def get_hw_status(dev) -> HWStatus:
    """Send GET_HW_STATUS and decode the response."""
    data, status = usb_transact(dev, GHS_CMD, expect_data=True)

    # If we got a 0x53 status instead of data, the command may have
    # been rejected or we have a protocol mismatch. Report it.
    if data and data[0] == 0x53 and len(data) == 13:
        print(f"  WARNING: got status 0x53 instead of data. Trying data from status phase...")
        if status and len(status) >= 12:
            data = status

    return HWStatus.from_response(data)


# --- Modes ---

def mode_once(dev):
    """Single read with full diagnostic dump."""
    print("=== TEST UNIT READY ===")
    ready = test_unit_ready(dev)
    print(f"Device ready: {ready}\n")

    print("=== GET_HW_STATUS ===")
    print(f"Command: {hex_of(GHS_CMD)}\n")

    data, status = usb_transact(dev, GHS_CMD, expect_data=True)
    print(f"Phase 2 (data):   [{len(data):2d} bytes] {hex_of(data)}")
    if status:
        print(f"Phase 3 (status): [{len(status):2d} bytes] {hex_of(status)}")
    print()

    hw = HWStatus.from_response(data)
    print("Decoded status:")
    print(f"  hopper (paper in feeder): {hw.hopper}")
    print(f"  adf_open (door open):     {hw.adf_open}")
    print(f"  paper_end:                {hw.paper_end}")
    print(f"  scan_sw (button):         {hw.scan_sw}")
    print(f"  manual_feed:              {hw.manual_feed}")
    print(f"  send_sw:                  {hw.send_sw}")
    print()
    print("Raw bytes of interest:")
    for i, label in [(3, "ADF/paper"), (4, "buttons"), (5, "consumables"), (6, "errors")]:
        b = hw.raw[i] if i < len(hw.raw) else 0
        print(f"  byte[{i}] ({label:12s}): 0x{b:02x} = {bits_of(b)}")


def mode_raw(dev):
    """Continuous raw hex output, one line per poll."""
    print(f"{'time':>12s} | {'raw hex':40s} | hop adf btn")
    print("-" * 72)
    while True:
        try:
            hw = get_hw_status(dev)
            ts = time.strftime("%H:%M:%S") + f".{int(time.time() * 10) % 10}"
            print(
                f"{ts:>12s} | {hex_of(hw.raw):40s} | "
                f"{'Y' if hw.hopper else '.'} "
                f"{'O' if hw.adf_open else '.'} "
                f"{'B' if hw.scan_sw else '.'}"
            )
            time.sleep(0.1)
        except KeyboardInterrupt:
            break


def mode_monitor(dev):
    """Print only state changes. Default mode."""
    print("Monitoring for state changes... (Ctrl-C to stop)")
    print("Try: press scan button, insert paper, remove paper, open/close ADF")
    print()

    prev = None
    poll_count = 0
    while True:
        try:
            hw = get_hw_status(dev)
            poll_count += 1

            if prev is None:
                ts = time.strftime("%H:%M:%S")
                print(f"[{ts}] Initial state (poll #{poll_count}):")
                print(f"  hopper={hw.hopper}  adf_open={hw.adf_open}  scan_sw={hw.scan_sw}")
                print(f"  raw: {hex_of(hw.raw)}")
                for i in range(3, 7):
                    b = hw.raw[i] if i < len(hw.raw) else 0
                    print(f"  byte[{i}]: 0x{b:02x} = {bits_of(b)}")
                print()
            else:
                changes = hw.diff(prev)
                if changes:
                    ts = time.strftime("%H:%M:%S")
                    print(f"[{ts}] STATE CHANGE (poll #{poll_count}):")
                    for c in changes:
                        print(c)
                    print(f"  raw: {hex_of(hw.raw)}")
                    print()

            prev = hw
            time.sleep(0.1)
        except KeyboardInterrupt:
            print(f"\nDone. {poll_count} polls.")
            break
        except usb.core.USBError as e:
            print(f"USB error: {e} — retrying in 1s...", file=sys.stderr)
            time.sleep(1)


def mode_discover(dev):
    """
    Guided discovery mode: prompts you through physical actions
    and records which bits flip for each one.
    """
    print("=== BIT DISCOVERY MODE ===")
    print("We'll record the raw response, then ask you to change something,")
    print("then record again and show you exactly which bits changed.\n")

    actions = [
        ("baseline — don't touch anything", 3),
        ("PRESS and HOLD the scan button", 3),
        ("RELEASE the scan button", 3),
        ("INSERT a sheet of paper into the feeder", 5),
        ("REMOVE the sheet of paper", 5),
    ]

    snapshots: list[tuple[str, HWStatus]] = []

    for description, settle_secs in actions:
        input(f">>> {description}, then press Enter... ")
        print(f"  reading (waiting {settle_secs}s for settle)...")
        time.sleep(0.5)

        # Take several readings to confirm stability
        readings = []
        for _ in range(settle_secs * 10):
            readings.append(get_hw_status(dev))
            time.sleep(0.1)

        # Use last stable reading
        hw = readings[-1]
        snapshots.append((description, hw))
        print(f"  raw: {hex_of(hw.raw)}")
        for i in range(12):
            b = hw.raw[i] if i < len(hw.raw) else 0
            print(f"    [{i:2d}] 0x{b:02x} = {bits_of(b)}")
        print()

    # Diff report
    print("=" * 60)
    print("DIFF REPORT")
    print("=" * 60)
    baseline = snapshots[0][1]
    for desc, hw in snapshots[1:]:
        changes = hw.diff(baseline)
        print(f"\n'{desc}' vs baseline:")
        if changes:
            for c in changes:
                print(f"  {c}")
        else:
            print("  (no changes)")


# --- Main ---

def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "--monitor"

    print(f"ScanSnap S1500 Explorer")
    print(f"USB: {VID:#06x}:{PID:#06x}  EP_OUT={EP_OUT:#04x}  EP_IN={EP_IN:#04x}")
    print()

    dev = open_scanner()
    print(f"Connected.\n")

    try:
        modes = {
            "--once": mode_once,
            "--raw": mode_raw,
            "--monitor": mode_monitor,
            "--discover": mode_discover,
        }
        fn = modes.get(mode)
        if fn is None:
            print(f"Unknown mode: {mode}")
            print(f"Available: {', '.join(modes)}")
            sys.exit(1)
        fn(dev)
    finally:
        usb.util.release_interface(dev, 0)
        try:
            dev.attach_kernel_driver(0)
        except (usb.core.USBError, NotImplementedError):
            pass


if __name__ == "__main__":
    main()
