# ScanSnap S1500 USB Protocol

Reverse-engineered protocol documentation for the Fujitsu ScanSnap S1500,
derived from USB captures, SANE `fujitsu` backend source analysis, and
empirical testing with `docs/explore.py`.

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
| 3    | 7   | `0x80` | Hopper empty — **inverted**: 1 = empty, 0 = paper present |
| 4    | 5   | `0x20` | Scan button physically held down |
| 4    | 0   | `0x01` | Scan button momentary/tap (set transiently for ~1 poll cycle) |
| 4    | 7   | `0x80` | "Virgin" flag — set at power-on, clears permanently on first button press |

**Important:** Quick taps only set bit 0 (0x01) transiently for about one poll cycle.
Long holds set bit 5 (0x20). The daemon checks both with mask `0x21`.

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

## SANE bit-map discrepancy

The SANE `fujitsu` backend header defines the scan button as bit 0 of byte 4.
This was found to be **incorrect for the S1500** — the sustained-hold button
signal is bit 5. Bit 0 is a transient tap indicator. The empirical test via
`docs/explore.py --discover` was essential for verifying the correct mapping.

## Diagnostic tool

`docs/explore.py` is a Python USB explorer (requires `pyusb`) with four modes:

- `--once`: single read with full hex dump
- `--raw`: continuous hex output
- `--monitor`: state-change detection (default)
- `--discover`: guided bit-mapping walkthrough

Use it to verify bit mapping on other ScanSnap models.
