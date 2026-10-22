#!/usr/bin/env python3
import sys
import unicodedata

assert unicodedata.unidata_version == "15.1.0", unicodedata.unidata_version

EXCLUDED = (
    set(range(0xFE00, 0xFE10))
    | set(range(0xE0100, 0xE01F0))
    | {0x34F, 0x17B4, 0x17B5, 0x20E3}
    | set(range(0x180B, 0x180F))
    | set(range(0xE0000, 0xE0080))
)

def compress(codepoints):
    ranges = []
    start = prev = None
    for cp in codepoints:
        if start is None:
            start = prev = cp
        elif cp == prev + 1:
            prev = cp
        else:
            ranges.append((start, prev))
            start = prev = cp
    ranges.append((start, prev))
    return ranges


def emit(name, categories, excluded):
    codepoints = [
        cp
        for cp in range(0x110000)
        if unicodedata.category(chr(cp)) in categories and cp not in excluded
    ]
    ranges = compress(codepoints)
    sys.stderr.write(f"{name} unicode {unicodedata.unidata_version}: {len(codepoints)} code points, {len(ranges)} ranges\n")
    print(f"const {name}: &[(u32, u32)] = &[")
    for lo, hi in ranges:
        print(f"    (0x{lo:x}, 0x{hi:x}),")
    print("];")
    print()


emit("ALNUM_RANGES", ("Lu", "Ll", "Lt", "Lm", "Lo", "Nd", "Nl", "No"), set())
emit("MARK_RANGES", ("Mn", "Mc", "Me"), EXCLUDED)
