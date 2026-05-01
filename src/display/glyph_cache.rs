use crate::font::{Font, GlyphInfo};

const GLYPH_CACHE_CAPACITY: usize = 128;

struct GlyphCacheEntry {
    font_id: usize,
    codepoint: u32,
    bitmap: Vec<u8>,
    last_used: u32,
}

pub(super) struct GlyphCache {
    entries: Vec<GlyphCacheEntry>,
    clock: u32,
}

impl GlyphCache {
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::with_capacity(GLYPH_CACHE_CAPACITY),
            clock: 0,
        }
    }

    pub(super) fn get_or_insert(
        &mut self,
        font: &Font,
        codepoint: u32,
        info: &GlyphInfo,
        scratch: &mut [u8],
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

        if font.decompress_glyph(info, scratch).is_err() {
            return None;
        }

        let size = Font::glyph_uncompressed_size(info);
        let bitmap = scratch[..size].to_vec();
        let entry = GlyphCacheEntry {
            font_id,
            codepoint,
            bitmap,
            last_used: now,
        };

        let index = if self.entries.len() < GLYPH_CACHE_CAPACITY {
            self.entries.push(entry);
            self.entries.len() - 1
        } else {
            let index = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| index)
                .unwrap_or(0);
            self.entries[index] = entry;
            index
        };

        Some(&self.entries[index].bitmap)
    }
}
