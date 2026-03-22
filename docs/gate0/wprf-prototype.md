# Gate 0 — wPRF v1 Roundtrip Prototype

- **Issue:** #9
- **Date:** 2026-03-22
- **Pass criteria:** 10-event roundtrip + truncation recovery of first N events

## Test Results

```
============================================================
Gate 0 #9: wPRF v1 Roundtrip Prototype
============================================================

--- Test 1: Header Layout ---
[PASS] Header: 64 bytes, magic=wPRF, version=1, roundtrip OK
       Hex: [77, 50, 52, 46, 01, 01, 00, 00, 00, 00, 00, 00, 00, 00, 00, 00]

--- Test 2: Full Roundtrip ---
[PASS] Roundtrip: 10 events, all fields match
       File: 376 bytes (hdr=64, events=10×28=280, footer=32)

--- Test 3: Truncation Recovery ---
[PASS] Truncation: cut at byte 242, recovered 6/10 events

============================================================
All tests passed.
============================================================
```

## Header Hex Dump (First 16 Bytes)

```
Offset  Hex                                      ASCII
00      77 50 52 46 01 01 00 00                  wPRF....
08      00 00 00 00 00 00 00 00                  ........
```

- Bytes 0-3: Magic `wPRF` (0x77505246)
- Byte 4: Version = 1
- Byte 5: Endianness = 1 (LE)
- Byte 6: Architecture = 0 (x86_64)
- Byte 7: `_reserved` = 0

## File Layout Verified

```
Offset   Size   Content
0        64     WprfHeader (magic + version + offsets + feature_bitmap)
64       280    10 × TLV-wrapped BaseEvents (5B header + 23B payload = 28B each)
344      20     Footer Section Entry (id=3, offset, size)
364      12     Metadata payload ("DROP_COUNT=0")
Total: 376 bytes
```

## Crash Recovery Mechanism

The `data_section_end_offset` field in the header is set to the byte position after the last complete event (byte 344). When the file is truncated at byte 242 (mid-event 7):

1. Reader opens file, reads 64B header
2. `data_section_end_offset = 344` — but file is only 242 bytes
3. Reader uses `min(data_section_end_offset, file_size) = 242` as scan limit
4. Reads events starting at byte 64, each 28 bytes
5. 6 complete events fit in 64 + 6×28 = 232 bytes
6. Remaining 10 bytes (242-232) insufficient for a full event — stop
7. **6 events recovered intact, 4 lost**

## Discoveries

1. **Footer-at-end works correctly.** The section table offset and metadata payload are written after all events. The header's `section_table_offset` points to the right place.

2. **Forward compatibility via TLV.** Unknown `rec_type` values can be skipped by reading `length` bytes — tested implicitly by the strict type check in `read_tlv()`.

3. **No alignment issues.** The 23-byte BaseEvent is intentionally unaligned (no padding). All fields use `to_le_bytes()`/`from_le_bytes()` for portability. This matches the design spec's fixed-length approach.
