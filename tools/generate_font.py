#!/usr/bin/env python3
"""Generate a compressed bitmap font binary for the rr_reader e-ink firmware.

Usage:
    python3 generate_font.py <ttf_path> <output_bin> [--size PX] [--profile full|math] [--chars-file CHARS.txt] [--oversample N] [--dither] [--shrink F]

Output: a binary blob consumed by src/font.rs via include_bytes!().

Optimizations over the original:
  - Oversampling: renders at N× resolution then Lanczos-downscales for
    smoother glyph edges. Default 4×; set --oversample 1 to disable.
  - Baseline alignment: uses font metrics to place glyphs on a consistent
    baseline instead of individually centering each glyph. The baseline
    position is stored in the binary so the runtime can use it.
  - Adaptive thresholding: adjusts the binarization threshold per glyph
    based on stroke density to avoid broken strokes in light glyphs and
    filled-in counters in heavy glyphs.
  - Shrink fitting (--shrink): slightly scales down the rendered glyph
    within its cell to prevent clipping at the top/bottom edges.
    Default 0.94 (94%).
  - Bayer ordered dithering (--dither): replaces simple thresholding with
    a 4×4 Bayer matrix comparison that simulates up to 17 gray levels on
    the 1-bit e-ink display.  Significantly smooths glyph edges at the
    cost of slightly reduced zlib compression ratio.

Character selection:
    The default "full" profile generates glyphs for key Unicode blocks needed for Chinese text:
    - Basic Latin (U+0020..U+007E)
    - Latin-1 Supplement, selected (U+00A0..U+00FF)
    - Greek and Coptic (U+0370..U+03FF)
    - Superscripts and Subscripts (U+2070..U+209F)
    - Letterlike Symbols (U+2100..U+214F)
    - CJK Symbols and Punctuation (U+3000..U+303F)
    - Hiragana (U+3040..U+309F)
    - Katakana (U+30A0..U+30FF)
    - CJK Unified Ideographs, subset (U+4E00..U+5FFF, first 8192)
    - Halfwidth/Fullwidth Forms (U+FF00..U+FFEF)
    The "math" profile generates a much smaller Latin/Greek/symbol set for formulas and scripts.
    Use --chars-file to add extra characters from a UTF-8 text file.
"""

import argparse
import struct
import sys
import time
import zlib
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

# ── Bayer ordered dithering matrix (4×4) ─────────────────────────────
# Maps 0..15 → each pixel compared against threshold = (value * 16 + 8),
# producing 17 perceived gray levels on a 1-bit display.
#
#   0   8   2  10
#  12   4  14   6
#   3  11   1   9
#  15   7  13   5
BAYER_4X4: list[list[int]] = [
    [0, 8, 2, 10],
    [12, 4, 14, 6],
    [3, 11, 1, 9],
    [15, 7, 13, 5],
]


# ── Binary format constants ──────────────────────────────────────────

MAGIC = b"FNT2"
INDEX_ENTRY_SIZE = 16  # codepoint:u32 + offset:u32 + compressed_size:u16 + uncompressed_size:u16 + advance:u8 + baseline:u8 + reserved:2

# Index entry layout (offsets within the 16-byte entry):
#  [ 0.. 4] codepoint: u32 LE
#  [ 4.. 8] data_offset: u32 LE
#  [ 8..10] compressed_size: u16 LE
#  [10..12] uncompressed_size: u16 LE
#  [12]     advance: u8
#  [13]     baseline: u8  ← NEW (was reserved)
#  [14..16] reserved: 2 bytes


# ── Unicode block ranges ─────────────────────────────────────────────


def unicode_block_ranges(profile: str):
    """Return list of (start, end) inclusive codepoint ranges to include."""
    shared = [
        # Basic Latin (printable)
        (0x0020, 0x007E),
        # Latin-1 Supplement (selected — cover accents, punctuation)
        (0x00A0, 0x00FF),
        # General Punctuation (selected)
        (0x2010, 0x2027),
        (0x2030, 0x205E),
        # Superscripts/Subscripts and symbols common in math notes
        (0x0370, 0x03FF),
        (0x2070, 0x209F),
        (0x2100, 0x214F),
        # Arrows
        (0x2190, 0x21FF),
        # Mathematical Operators
        (0x2200, 0x22FF),
    ]
    if profile == "math":
        return shared

    return shared + [
        # CJK Symbols and Punctuation
        (0x3000, 0x303F),
        # Hiragana
        (0x3040, 0x309F),
        # Katakana
        (0x30A0, 0x30FF),
        # Bopomofo
        (0x3100, 0x312F),
        # Box Drawing (UI borders)
        (0x2500, 0x257F),
        # CJK Unified Ideographs — full block (most common-use characters)
        (0x4E00, 0x9FFF),
        # Halfwidth and Fullwidth Forms
        (0xFF00, 0xFFEF),
    ]


def chars_from_ranges(profile: str):
    """Generate sorted unique characters from the configured Unicode ranges."""
    seen = set()
    for lo, hi in unicode_block_ranges(profile):
        for cp in range(lo, hi + 1):
            # Skip surrogates and invalid codepoints
            if 0xD800 <= cp <= 0xDFFF:
                continue
            ch = chr(cp)
            if ch not in seen:
                seen.add(ch)
    return sorted(seen, key=ord)


# ── Font metrics ─────────────────────────────────────────────────────


def get_font_baseline(font: ImageFont.FreeTypeFont, size: int) -> int:
    """Return the baseline position (px from top of cell) for this font.

    Uses getmetrics() (Pillow ≥9.2) when available, otherwise falls back to
    a heuristic based on the 'x' glyph bounding box.
    """
    try:
        ascent, descent = font.getmetrics()
        if ascent + descent > 0:
            # Proportionally scale baseline into the target cell, then push
            # it ~15% lower.  The design metrics often underestimate the
            # actual rendered ascent; extra headroom prevents top clipping.
            ratio = ascent / (ascent + descent)
            return min(int(size * ratio * 1.15), size - 1)
    except AttributeError:
        pass

    # Fallback: use 'x' bbox as a rough baseline indicator
    bbox = font.getbbox("x")
    if bbox is not None:
        _left, _top, _right, bottom = bbox
        return min(bottom, size - 1)

    # Last resort: 85% from top (conservative headroom)
    return int(size * 0.85)


# ── Glyph rendering ──────────────────────────────────────────────────


def render_glyph_oversampled(
    font: ImageFont.FreeTypeFont,
    ch: str,
    size: int,
    baseline_px: int,
    oversample: int,
    shrink: float,
) -> Image.Image:
    """Render a glyph with oversampling, returning a grayscale PIL Image.

    Renders at `size * oversample` resolution, then Lanczos-downscales to
    `size`.  If shrink < 1.0 the glyph is further scaled down and centered
    to leave a safety margin against cell-edge clipping.
    """
    big_size = size * oversample
    big_baseline = baseline_px * oversample

    # Load the font at the oversampled size
    big_font = ImageFont.truetype(font.path, big_size)

    img = Image.new("L", (big_size, big_size), 255)
    draw = ImageDraw.Draw(img)

    bbox = big_font.getbbox(ch)
    if bbox is not None:
        left, _top, _right, _bottom = bbox
        x = -left
    else:
        x = 0

    draw.text((x, big_baseline), ch, font=big_font, fill=0, anchor="ls")

    # Downscale with Lanczos (best quality for downscaling)
    if oversample > 1:
        img = img.resize((size, size), Image.LANCZOS)

    # Optional shrink to prevent clipping at cell edges
    if shrink < 1.0:
        glyph_px = max(4, round(size * shrink))
        img = img.resize((glyph_px, glyph_px), Image.LANCZOS)
        canvas = Image.new("L", (size, size), 255)
        offset = (size - glyph_px) // 2
        canvas.paste(img, (offset, offset))
        img = canvas

    return img


def render_glyph_direct(
    font: ImageFont.FreeTypeFont,
    ch: str,
    size: int,
    baseline_px: int,
    shrink: float,
) -> Image.Image:
    """Render a glyph at the target size (no oversampling)."""
    img = Image.new("L", (size, size), 255)
    draw = ImageDraw.Draw(img)

    bbox = font.getbbox(ch)
    if bbox is not None:
        left, _top, _right, _bottom = bbox
        x = -left
    else:
        x = 0

    draw.text((x, baseline_px), ch, font=font, fill=0, anchor="ls")

    # Optional shrink to prevent clipping at cell edges
    if shrink < 1.0:
        glyph_px = max(4, round(size * shrink))
        img = img.resize((glyph_px, glyph_px), Image.LANCZOS)
        canvas = Image.new("L", (size, size), 255)
        offset = (size - glyph_px) // 2
        canvas.paste(img, (offset, offset))
        img = canvas

    return img


def adaptive_threshold(img: Image.Image, size: int) -> int:
    """Choose a binarization threshold based on glyph stroke density.

    Returns a threshold in [85, 160]:
      - Light/sparse glyphs (e.g. '.', '-') → lower threshold to preserve thin strokes
      - Dense glyphs (e.g. CJK, '█') → higher threshold to avoid filling counters
    """
    pixels = list(img.getdata())
    total = len(pixels)

    # Count "ink" pixels at a generous threshold to estimate density
    dark_count = sum(1 for p in pixels if p < 200)
    density = dark_count / total

    # Map density to threshold:
    #   density < 5%  → threshold ~95  (very sparse: dots, hyphens)
    #   density ~20%  → threshold ~125 (typical Latin)
    #   density ~40%+ → threshold ~150 (dense CJK)
    if density < 0.03:
        threshold = 90
    elif density < 0.08:
        threshold = 100
    elif density < 0.15:
        threshold = 115
    elif density < 0.30:
        threshold = 128
    elif density < 0.50:
        threshold = 140
    else:
        threshold = 150

    return threshold


def binarize_image_threshold(img: Image.Image, size: int, threshold: int) -> bytearray:
    """Simple adaptive threshold binarization."""
    bytes_per_row = (size + 7) // 8
    bitmap = bytearray(bytes_per_row * size)

    for y in range(size):
        for x in range(size):
            pixel = img.getpixel((x, y))
            if pixel < threshold:
                byte_idx = y * bytes_per_row + x // 8
                bit_idx = 7 - (x % 8)
                bitmap[byte_idx] |= 1 << bit_idx

    return bitmap


def binarize_image_dithered(img: Image.Image, size: int) -> bytearray:
    """Bayer 4×4 ordered dithering.

    Instead of a single global threshold, each pixel is compared against a
    value from the Bayer matrix.  This produces a regular dither pattern
    that the human eye perceives as smooth gray transitions — effectively
    anti-aliasing on a 1-bit display.

    The Bayer thresholds range from 8 to 248 (centered on 128), giving
    17 distinct gray levels.
    """
    bytes_per_row = (size + 7) // 8
    bitmap = bytearray(bytes_per_row * size)

    for y in range(size):
        row_bayer = BAYER_4X4[y & 3]
        for x in range(size):
            pixel = img.getpixel((x, y))
            # Bayer value 0..15 → threshold 8..248
            bayer_threshold = row_bayer[x & 3] * 16 + 8
            if pixel < bayer_threshold:
                byte_idx = y * bytes_per_row + x // 8
                bit_idx = 7 - (x % 8)
                bitmap[byte_idx] |= 1 << bit_idx

    return bitmap


def glyph_bitmap(
    font: ImageFont.FreeTypeFont,
    ch: str,
    size: int,
    baseline_px: int,
    oversample: int,
    dither: bool,
    shrink: float,
) -> bytes:
    """Render, optionally oversample, binarize, and return packed bitmap."""
    if oversample > 1:
        img = render_glyph_oversampled(font, ch, size, baseline_px, oversample, shrink)
    else:
        img = render_glyph_direct(font, ch, size, baseline_px, shrink)

    if dither:
        bitmap = binarize_image_dithered(img, size)
    else:
        threshold = adaptive_threshold(img, size)
        bitmap = binarize_image_threshold(img, size, threshold)

    return bytes(bitmap)


def glyph_advance(font: ImageFont.FreeTypeFont, ch: str, size: int) -> int:
    """Return the horizontal cursor advance in pixels, matching proportional font metrics."""
    try:
        advance = font.getlength(ch)
    except AttributeError:
        advance = font.getsize(ch)[0]

    # Keep CJK and other full-width glyphs on a stable cell, while letting Latin
    # text use the font's proportional advance like crosspoint's advanceX path.
    cp = ord(ch)
    if (
        0x2E80 <= cp <= 0xA4CF
        or 0xAC00 <= cp <= 0xD7AF
        or 0xF900 <= cp <= 0xFAFF
        or 0xFE10 <= cp <= 0xFE6F
        or 0xFF00 <= cp <= 0xFF60
        or 0xFFE0 <= cp <= 0xFFE6
    ):
        return size

    if ch == " ":
        return max(1, round(advance))

    return max(1, min(size, round(advance)))


# ── Main generation ──────────────────────────────────────────────────


def generate(args):
    t0 = time.time()
    font = ImageFont.truetype(args.ttf_path, args.size)

    baseline_px = get_font_baseline(font, args.size)
    oversample = max(1, args.oversample)
    dither = args.dither
    shrink = args.shrink

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
        chars = chars_from_ranges(args.profile)

    glyph_count = len(chars)
    glyph_width = args.size
    glyph_height = args.size
    bytes_per_row = (glyph_width + 7) // 8
    uncompressed_glyph_bytes = bytes_per_row * glyph_height

    mode = "Bayer dither" if dither else "adaptive threshold"
    print(f"Generating {glyph_count} glyphs at {glyph_width}x{glyph_height}px...")
    print(f"  Oversample: {oversample}×")
    print(f"  Baseline:   {baseline_px}px from top")
    print(f"  Shrink:     {shrink:.0%}")
    print(f"  Binarize:   {mode}")

    # ── Build index and data blob ──────────────────────────────────
    index_entries = []
    data_offset = 8 + glyph_count * INDEX_ENTRY_SIZE  # header + index

    for i, ch in enumerate(chars):
        bitmap = glyph_bitmap(
            font, ch, args.size, baseline_px, oversample, dither, args.shrink
        )
        compressed = zlib.compress(bitmap, level=9)
        advance = glyph_advance(font, ch, args.size)

        index_entries.append(
            (ord(ch), data_offset, compressed, len(bitmap), advance, baseline_px)
        )
        data_offset += len(compressed)

        if (i + 1) % 500 == 0:
            elapsed = time.time() - t0
            rate = (i + 1) / elapsed
            remaining = (glyph_count - i - 1) / rate
            print(
                f"  {i + 1}/{glyph_count} glyphs ({rate:.0f}/s, ~{remaining:.0f}s remaining)"
            )

    # ── Write binary ───────────────────────────────────────────────
    with open(args.output_bin, "wb") as f:
        f.write(MAGIC)
        f.write(struct.pack("<B", glyph_width))
        f.write(struct.pack("<B", glyph_height))
        f.write(struct.pack("<H", glyph_count))

        for (
            codepoint,
            offset,
            compressed,
            uncomp_size,
            advance,
            baseline,
        ) in index_entries:
            f.write(struct.pack("<I", codepoint))  # [ 0.. 4]
            f.write(struct.pack("<I", offset))  # [ 4.. 8]
            f.write(struct.pack("<H", len(compressed)))  # [ 8..10]
            f.write(struct.pack("<H", uncomp_size))  # [10..12]
            f.write(struct.pack("<B", advance))  # [12]
            f.write(struct.pack("<B", baseline))  # [13] ← baseline offset
            f.write(b"\x00\x00")  # [14..16] reserved

        for _, _, compressed, _, _, _ in index_entries:
            f.write(compressed)

    total_size = (
        8
        + glyph_count * INDEX_ENTRY_SIZE
        + sum(len(c) for _, _, c, _, _, _ in index_entries)
    )

    # ── Summary ────────────────────────────────────────────────────
    elapsed = time.time() - t0
    index_kb = glyph_count * INDEX_ENTRY_SIZE / 1024
    data_kb = sum(len(c) for _, _, c, _, _, _ in index_entries) / 1024
    data_uncompressed_kb = sum(s for _, _, _, s, _, _ in index_entries) / 1024
    total_kb = total_size / 1024

    print(f"Font generated: {args.output_bin}")
    print(
        f"  Size:      {args.size}x{args.size}px ({uncompressed_glyph_bytes}B/glyph uncompressed)"
    )
    print(f"  Glyphs:    {glyph_count}")
    print(f"  Index:     {index_kb:.1f} KB")
    print(
        f"  Data:      {data_kb:.1f} KB (DEFLATE compressed, {data_uncompressed_kb:.1f} KB raw)"
    )
    print(f"  Total:     {total_kb:.1f} KB")
    print(f"  Time:      {elapsed:.1f}s")
    print(f"  First 20:  {repr(chars[:20])}")


def main():
    parser = argparse.ArgumentParser(description="Generate compressed bitmap font")
    parser.add_argument("ttf_path", help="Path to TrueType/OpenType font file")
    parser.add_argument("output_bin", help="Output binary file path")
    parser.add_argument(
        "--size", type=int, default=16, help="Glyph size in pixels (default: 16)"
    )
    parser.add_argument(
        "--profile",
        choices=("full", "math"),
        default="full",
        help="Character profile to generate (default: full)",
    )
    parser.add_argument(
        "--chars-file", help="Optional UTF-8 file with additional characters"
    )
    parser.add_argument(
        "--oversample",
        type=int,
        default=4,
        help="Oversampling factor: render at N× size then downscale (default: 4, set 1 to disable)",
    )
    parser.add_argument(
        "--dither",
        action="store_true",
        default=False,
        help="Use Bayer 4x4 ordered dithering instead of adaptive thresholding",
    )
    parser.add_argument(
        "--shrink",
        type=float,
        default=0.94,
        help="Scale glyph down within cell to prevent edge clipping (default: 0.94, range: 0.80..1.0)",
    )
    args = parser.parse_args()

    if not Path(args.ttf_path).exists():
        print(f"Error: font file not found: {args.ttf_path}", file=sys.stderr)
        sys.exit(1)

    if args.oversample < 1 or args.oversample > 8:
        print(
            f"Error: --oversample must be 1..8, got {args.oversample}", file=sys.stderr
        )
        sys.exit(1)

    if args.shrink < 0.8 or args.shrink > 1.0:
        print(f"Error: --shrink must be 0.8..1.0, got {args.shrink}", file=sys.stderr)
        sys.exit(1)

    generate(args)


if __name__ == "__main__":
    main()
