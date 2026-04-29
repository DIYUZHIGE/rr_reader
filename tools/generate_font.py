#!/usr/bin/env python3
"""Generate a compressed bitmap font binary for the rr_reader e-ink firmware.

Usage:
    python3 generate_font.py <ttf_path> <output_bin> [--size PX] [--chars-file CHARS.txt]

Output: a binary blob consumed by src/font.rs via include_bytes!().

Character selection:
    By default, generates glyphs for key Unicode blocks needed for Chinese text:
    - Basic Latin (U+0020..U+007E)
    - Latin-1 Supplement, selected (U+00A0..U+00FF)
    - CJK Symbols and Punctuation (U+3000..U+303F)
    - Hiragana (U+3040..U+309F)
    - Katakana (U+30A0..U+30FF)
    - CJK Unified Ideographs, subset (U+4E00..U+5FFF, first 8192)
    - Halfwidth/Fullwidth Forms (U+FF00..U+FFEF)
    Use --chars-file to add extra characters from a UTF-8 text file.
"""

import argparse
import struct
import sys
import time
import zlib
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


# ── Binary format constants ──────────────────────────────────────────

MAGIC = b"FONT"
INDEX_ENTRY_SIZE = 12  # codepoint:u32 + offset:u32 + compressed_size:u16 + uncompressed_size:u16


# ── Unicode block ranges ─────────────────────────────────────────────

def unicode_block_ranges():
    """Return list of (start, end) inclusive codepoint ranges to include."""
    return [
        # Basic Latin (printable)
        (0x0020, 0x007E),
        # Latin-1 Supplement (selected — cover accents, punctuation)
        (0x00A0, 0x00FF),
        # General Punctuation (selected)
        (0x2010, 0x2027),
        (0x2030, 0x205E),
        # CJK Symbols and Punctuation
        (0x3000, 0x303F),
        # Hiragana
        (0x3040, 0x309F),
        # Katakana
        (0x30A0, 0x30FF),
        # Bopomofo
        (0x3100, 0x312F),
        # Arrows
        (0x2190, 0x21FF),
        # Mathematical Operators
        (0x2200, 0x22FF),
        # Box Drawing (UI borders)
        (0x2500, 0x257F),
        # CJK Unified Ideographs — full block (most common-use characters)
        (0x4E00, 0x9FFF),
        # Halfwidth and Fullwidth Forms
        (0xFF00, 0xFFEF),
    ]


def chars_from_ranges():
    """Generate sorted unique characters from the configured Unicode ranges."""
    chars = []
    seen = set()
    for lo, hi in unicode_block_ranges():
        for cp in range(lo, hi + 1):
            # Skip surrogates and invalid codepoints
            if 0xD800 <= cp <= 0xDFFF:
                continue
            ch = chr(cp)
            if ch not in seen:
                seen.add(ch)
                chars.append(ch)
    return chars


def glyph_bitmap(font: ImageFont.FreeTypeFont, ch: str, size: int) -> bytes:
    """Render a single glyph and return its row-major bitmap bytes (MSB first)."""
    img = Image.new("L", (size, size), 255)
    draw = ImageDraw.Draw(img)
    draw.text((0, 0), ch, font=font, fill=0)

    bytes_per_row = (size + 7) // 8
    bitmap = bytearray(bytes_per_row * size)
    for y in range(size):
        for x in range(size):
            pixel = img.getpixel((x, y))
            if pixel < 128:  # dark pixel
                byte_idx = y * bytes_per_row + x // 8
                bit_idx = 7 - (x % 8)
                bitmap[byte_idx] |= 1 << bit_idx
    return bytes(bitmap)


def generate(args):
    t0 = time.time()
    font = ImageFont.truetype(args.ttf_path, args.size)

    # Collect characters
    if args.chars_file:
        chars = []
        seen = set()
        # Always include ASCII
        for cp in range(0x20, 0x7F):
            ch = chr(cp)
            seen.add(ch)
            chars.append(ch)
        with open(args.chars_file, "r", encoding="utf-8") as f:
            for ch in f.read():
                if ch not in seen:
                    seen.add(ch)
                    chars.append(ch)
        chars.sort(key=lambda c: ord(c))
    else:
        chars = chars_from_ranges()

    glyph_count = len(chars)
    glyph_width = args.size
    glyph_height = args.size
    bytes_per_row = (glyph_width + 7) // 8
    uncompressed_glyph_bytes = bytes_per_row * glyph_height

    print(f"Generating {glyph_count} glyphs at {glyph_width}x{glyph_height}px...")

    # ── Build index and data blob ──────────────────────────────────
    index_entries = []
    data_offset = 8 + glyph_count * INDEX_ENTRY_SIZE  # header + index

    for i, ch in enumerate(chars):
        bitmap = glyph_bitmap(font, ch, args.size)
        compressed = zlib.compress(bitmap, level=9)

        index_entries.append(
            (ord(ch), data_offset, compressed, len(bitmap))
        )
        data_offset += len(compressed)

        if (i + 1) % 500 == 0:
            elapsed = time.time() - t0
            rate = (i + 1) / elapsed
            remaining = (glyph_count - i - 1) / rate
            print(f"  {i+1}/{glyph_count} glyphs ({rate:.0f}/s, ~{remaining:.0f}s remaining)")

    # ── Write binary ───────────────────────────────────────────────
    with open(args.output_bin, "wb") as f:
        f.write(MAGIC)
        f.write(struct.pack("<B", glyph_width))
        f.write(struct.pack("<B", glyph_height))
        f.write(struct.pack("<H", glyph_count))

        for codepoint, offset, compressed, uncomp_size in index_entries:
            f.write(struct.pack("<I", codepoint))
            f.write(struct.pack("<I", offset))
            f.write(struct.pack("<H", len(compressed)))
            f.write(struct.pack("<H", uncomp_size))

        for _, _, compressed, _ in index_entries:
            f.write(compressed)

    total_size = 8 + glyph_count * INDEX_ENTRY_SIZE + sum(
        len(c) for _, _, c, _ in index_entries
    )

    # ── Summary ────────────────────────────────────────────────────
    elapsed = time.time() - t0
    index_kb = glyph_count * INDEX_ENTRY_SIZE / 1024
    data_kb = sum(len(c) for _, _, c, _ in index_entries) / 1024
    total_kb = total_size / 1024

    print(f"Font generated: {args.output_bin}")
    print(f"  Size:   {args.size}x{args.size}px ({uncompressed_glyph_bytes} bytes/glyph uncompressed)")
    print(f"  Glyphs: {glyph_count}")
    print(f"  Index:  {index_kb:.1f} KB")
    print(f"  Data:   {data_kb:.1f} KB (DEFLATE compressed)")
    print(f"  Total:  {total_kb:.1f} KB")
    print(f"  Time:   {elapsed:.1f}s")
    print(f"  First 20 chars: {repr(chars[:20])}")


def main():
    parser = argparse.ArgumentParser(description="Generate compressed bitmap font")
    parser.add_argument("ttf_path", help="Path to TrueType/OpenType font file")
    parser.add_argument("output_bin", help="Output binary file path")
    parser.add_argument("--size", type=int, default=16, help="Glyph size in pixels (default: 16)")
    parser.add_argument("--chars-file", help="Optional UTF-8 file with additional characters")
    args = parser.parse_args()

    if not Path(args.ttf_path).exists():
        print(f"Error: font file not found: {args.ttf_path}", file=sys.stderr)
        sys.exit(1)

    generate(args)


if __name__ == "__main__":
    main()
