---
layout: default
title: USB protocol reference
---

# ScanSnap S1500 USB Protocol

## Why direct USB?

The Fujitsu ScanSnap S1500 uses a vendor-specific USB protocol (class `FF:FF:FF`) — not standard USB mass storage or any other standard class. The SANE `fujitsu` backend wraps this protocol behind SANE's abstraction layer, which means polling for a single button press requires opening the full SANE stack: initialization, capability queries, option reads — about 25 SCSI commands per cycle.

All we actually need is one command (`GET_HW_STATUS`) and 12 bytes of response. Sending that directly via libusb eliminates the overhead entirely.

## How the protocol was discovered

The protocol was reverse-engineered through three complementary approaches:

1. **SANE source analysis** — The `fujitsu` backend in [sane-backends](https://gitlab.com/sane-project/backends) defines the USB envelope constants (`USB_COMMAND_CODE = 0x43`, `USB_COMMAND_LEN = 0x1F`, `USB_COMMAND_OFFSET = 0x13`) and the `GET_HW_STATUS` SCSI opcode (`0xC2`). This gave us the command structure.

2. **USB captures** — Wireshark captures of scanbd talking to the scanner confirmed the 3-phase protocol (command, data, status) and revealed the 12-byte response format.

3. **Empirical testing** — The included `explore.py` tool was used to systematically test each bit in the response by prompting a human operator to press the button, insert paper, etc. while recording which bits changed. This was essential because the SANE header's bit positions turned out to be wrong for the S1500 (see [SANE discrepancy](#sane-bit-map-discrepancy) below).

## USB device

- **VID:PID** = `04c5:11a2`
- **USB 2.0 High Speed**, self-powered
- **Two bulk endpoints only** — EP 1 IN (`0x81`), EP 2 OUT (`0x02`), 512-byte max packet
- **No interrupt endpoints** — polling is the only option
- **Vendor-specific class** `FF:FF:FF` — not standard SCSI, but SCSI-like

## Fujitsu USB wrapper protocol

Commands are wrapped in a **31-byte envelope**:

```
byte 0:     0x43  (Fujitsu USB_COMMAND_CODE)
bytes 1-18: 0x00  (padding)
bytes 19+:  SCSI CDB (up to 12 bytes)
```

The protocol is **3-phase**:
1. Write 31-byte command → EP_OUT (0x02)
2. Read data response → EP_IN (0x81)
3. Read status envelope → EP_IN (13 bytes starting with 0x53 = success)

These constants were confirmed by cross-referencing the SANE `fujitsu` backend:
- `USB_COMMAND_CODE = 0x43`, `USB_COMMAND_LEN = 0x1F (31)`, `USB_COMMAND_OFFSET = 0x13 (19)`

## GET_HW_STATUS command

SCSI opcode `0xC2`, 10-byte CDB: `C2 00 00 00 00 00 00 00 0C 00`

Full 31-byte envelope:
```
43 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 C2 00 00 00 00 00 00 00 0C 00 00 00
```

Returns **12 bytes**. Empirically verified bit mapping:

| Byte | Bit | Mask   | Meaning |
|------|-----|--------|---------|
| 3    | 7   | `0x80` | Hopper empty (see note below) |
| 4    | 5   | `0x20` | Scan button physically held down |
| 4    | 0   | `0x01` | Scan button momentary/tap (set transiently for ~1 poll cycle) |
| 4    | 7   | `0x80` | "Virgin" flag (see note below) |

### Hopper bit (byte 3, bit 7)

This bit is **inverted from what you'd expect**: `1` means the hopper is **empty**, `0` means paper is **present**. This is a "hopper empty" flag, not a "paper present" flag. The daemon inverts it internally so the `paper` field means what you'd think it means.

### Two button bits (byte 4, bits 0 and 5)

The scanner reports button state through two different bits depending on how the button is pressed:

- **Bit 5 (`0x20`)** — set while the button is physically held down. This is the sustained-hold signal.
- **Bit 0 (`0x01`)** — set transiently for approximately one poll cycle on a quick tap. If you're only checking bit 5, you'll miss quick taps entirely.

The daemon uses mask `0x21` to catch both behaviors. This is one of the key findings that differs from the SANE header (which only documents bit 0).

### Virgin flag (byte 4, bit 7)

Bit 7 of byte 4 (`0x80`) is set when the scanner first powers on and has never had its button pressed. It clears permanently after the first button press and stays cleared until the next power cycle (lid close/open). The daemon ignores this bit — it's not useful for event detection.

### Example responses

```
Baseline (no paper, button untouched):  00 00 00 80 80 01 80 00 00 00 00 00
Button held:                             00 00 00 80 20 01 80 00 00 00 00 00
Button released:                         00 00 00 80 00 01 80 00 00 00 00 00
Paper inserted:                          00 00 00 00 00 01 80 00 00 00 00 00
Paper removed:                           00 00 00 80 00 01 80 00 00 00 00 00
```

Bytes 5 (`0x01`) and 6 (`0x80`) were static across all tests — likely
consumable/error flags irrelevant to normal operation.

## Door state

**Door state is not reported in GET_HW_STATUS.** Opening the ADF lid powers
the scanner on (USB enumeration), closing it powers it off (USB disconnect).
Door state = USB device presence.

This is why the daemon has an outer loop that watches for USB connect/disconnect and an inner loop that polls `GET_HW_STATUS` — they're fundamentally different detection mechanisms.

## SANE bit-map discrepancy

The SANE `fujitsu` backend header defines the scan button as **bit 0 of byte 4**. Empirical testing with the S1500 shows this is incomplete:

- **Bit 0 (`0x01`)** is a transient tap indicator — it's set for roughly one poll cycle on a quick press, then clears automatically. This is what SANE documents.
- **Bit 5 (`0x20`)** is the sustained-hold signal — it stays set as long as the button is physically held down. This is **not documented** in the SANE header.

If a scanner daemon only checks bit 0 (as the SANE header suggests), it will detect quick taps but may miss them if the poll interval is too wide. If it only checks bit 5, it will detect holds but miss quick taps entirely. The correct approach for the S1500 is to check both with mask `0x21`.

This discrepancy was verified using `explore.py --discover`, which guides a human through pressing the button in different ways while recording raw hex responses. The SANE header may be correct for other Fujitsu models — the bit mapping could vary by device.

## Reproducing this on other models

If you have a different ScanSnap model, you can map its hardware status bits using the included Python diagnostic tool:

```sh
python3 docs/explore.py --discover
```

This runs a guided walkthrough:
1. Takes a baseline reading with no paper and button untouched
2. Asks you to insert paper, records which bits changed
3. Asks you to press and hold the button, records which bits changed
4. Asks you to tap the button quickly, records which bits changed

The tool requires `pyusb` (`pip install pyusb`) and root access (or appropriate udev rules). It handles kernel driver detachment and USB reset automatically.

Other useful modes:
- `--once` — single read with full hex dump (good for a quick sanity check)
- `--raw` — continuous hex output (good for watching raw changes)
- `--monitor` — state-change detection, shows only when bytes change

If you map a new model, please open a PR adding your findings to this document.

## Diagnostic tool

`docs/explore.py` is a Python USB explorer (requires `pyusb`) with four modes:

- `--once`: single read with full hex dump
- `--raw`: continuous hex output
- `--monitor`: state-change detection (default)
- `--discover`: guided bit-mapping walkthrough

Use it to verify bit mapping on other ScanSnap models.
