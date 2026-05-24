use crate::font::{Font, GlyphInfo};
use flate2::Decompress;

const GLYPH_CACHE_CAPACITY: usize = 12;

struct GlyphCacheEntry {
    font_id: usize,
    codepoint: u32,
    bitmap: Vec<u8>,
    last_used: u32,
}

pub(super) struct GlyphCache {
    entries: Vec<GlyphCacheEntry>,
    clock: u32,
    decomp: Decompress,
    scratch: Vec<u8>,
}

impl GlyphCache {
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::with_capacity(GLYPH_CACHE_CAPACITY),
            clock: 0,
            decomp: Decompress::new(true),
            scratch: Vec::new(),
        }
    }

    pub(super) fn get_or_insert(
        &mut self,
        font: &Font,
        codepoint: u32,
        info: &GlyphInfo,
    ) -> Option<&[u8]> {
        self.clock = self.clock.wrapping_add(1).max(1);
        let now = self.clock;
        let font_id = font.id();

        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.font_id == font_id && entry.codepoint == codepoint)
        {
            self.entries[index].last_used = now;
            return Some(&self.entries[index].bitmap);
        }

        let size = Font::glyph_uncompressed_size(info);
        if self.scratch.len() < size {
            self.scratch.resize(size, 0);
        }

        if font
            .decompress_glyph(&mut self.decomp, info, &mut self.scratch[..size])
            .is_err()
        {
            return None;
        }

        let bitmap = if self.entries.len() < GLYPH_CACHE_CAPACITY {
            // Under capacity: allocate a fresh bitmap.
            let mut v = Vec::with_capacity(size);
            v.extend_from_slice(&self.scratch[..size]);
            v
        } else {
            // Evict the LRU entry and reuse its allocation to avoid a free+alloc pair.
            let evict = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| index)
                .unwrap_or(0);
            let mut v = std::mem::take(&mut self.entries[evict].bitmap);
            v.clear();
            if v.capacity() < size {
                v.reserve_exact(size - v.capacity());
            }
            v.extend_from_slice(&self.scratch[..size]);
            self.entries[evict] = GlyphCacheEntry {
                font_id,
                codepoint,
                bitmap: v,
                last_used: now,
            };
            return Some(&self.entries[evict].bitmap);
        };

        let entry = GlyphCacheEntry {
            font_id,
            codepoint,
            bitmap,
            last_used: now,
        };
        self.entries.push(entry);
        Some(&self.entries[self.entries.len() - 1].bitmap)
    }

    /// Free all cached glyph bitmaps to reclaim heap for large allocations.
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.scratch.clear();
        self.scratch.shrink_to_fit();
    }
}
