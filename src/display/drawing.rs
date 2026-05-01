use super::{
    Display, DISPLAY_HEIGHT, DISPLAY_WIDTH, PHYSICAL_DISPLAY_HEIGHT, PHYSICAL_DISPLAY_WIDTH,
    PHYSICAL_DISPLAY_WIDTH_BYTES,
};
use crate::font::Font;
use crate::text::is_ascii_word_char;

impl Display {
    pub fn clear(&mut self, color: u8) {
        self.framebuffer.fill(color);
    }

    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u8) {
        self.fill_rect_pixels(x, y, w, h, color != 0x00);
        self.dirty = true;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn draw_text(&mut self, text: &str, x: usize, y: usize, scale: usize) {
        let mut cursor_x = x;
        for ch in text.chars() {
            self.draw_char(ch, cursor_x, y, scale);
            cursor_x += 6 * scale;
        }
        self.dirty = true;
    }

    /// Render UTF-8 text with the given font at (x, y). Missing glyphs are
    /// shown as an outline box so unsupported characters are visible.
    pub fn draw_text_font(&mut self, font: &Font, text: &str, x: usize, y: usize) {
        let mut cursor_x = x;
        let mut cursor_y = y;

        for ch in text.chars() {
            let cp = ch as u32;

            if cp == b'\n' as u32 {
                cursor_x = x;
                cursor_y += font.glyph_height as usize + 2;
                continue;
            }
            if cp == b'\r' as u32 {
                continue;
            }
            if cp == b'\t' as u32 || cp == b' ' as u32 {
                cursor_x += font.char_advance_width(ch);
                continue;
            }

            self.draw_font_char(font, ch, cursor_x, cursor_y);
            cursor_x += font.char_advance_width(ch);
        }
        self.dirty = true;
    }

    /// Render UTF-8 text with line wrapping. Returns the y coordinate after
    /// the last line rendered.
    pub fn draw_text_wrapped(
        &mut self,
        font: &Font,
        text: &str,
        x: usize,
        mut y: usize,
        max_x: usize,
        line_spacing: usize,
    ) -> usize {
        let start_x = x;
        let mut cursor_x = x;
        let line_height = font.glyph_height as usize + line_spacing;
        let line_width = max_x.saturating_sub(start_x).max(1);
        let mut iter = text.chars().peekable();
        let mut unit = String::with_capacity(32);

        while let Some(ch) = iter.next() {
            let cp = ch as u32;

            if cp == b'\n' as u32 {
                cursor_x = start_x;
                y += line_height;
                continue;
            }
            if cp == b'\r' as u32 {
                continue;
            }

            if y + font.glyph_height as usize > DISPLAY_HEIGHT {
                break;
            }

            unit.clear();
            unit.push(ch);
            if is_ascii_word_char(ch) {
                while let Some(&next) = iter.peek() {
                    if is_ascii_word_char(next) {
                        unit.push(next);
                        iter.next();
                    } else {
                        break;
                    }
                }
            }

            let unit_width = font.text_width(&unit);

            if unit.chars().all(|c| c == ' ' || c == '\t') {
                if cursor_x == start_x {
                    continue;
                }
                if cursor_x + unit_width > max_x {
                    cursor_x = start_x;
                    y += line_height;
                } else {
                    cursor_x += unit_width;
                }
                continue;
            }

            if cursor_x > start_x && cursor_x + unit_width > max_x {
                cursor_x = start_x;
                y += line_height;
            }

            if y + font.glyph_height as usize > DISPLAY_HEIGHT {
                break;
            }

            if unit_width <= line_width {
                self.draw_text_run(font, &unit, cursor_x, y);
                cursor_x += unit_width;
                continue;
            }

            for word_ch in unit.chars() {
                let advance = font.char_advance_width(word_ch);
                if cursor_x > start_x && cursor_x + advance > max_x {
                    cursor_x = start_x;
                    y += line_height;
                }
                if y + font.glyph_height as usize > DISPLAY_HEIGHT {
                    break;
                }
                self.draw_font_char(font, word_ch, cursor_x, y);
                cursor_x += advance;
            }
        }
        self.dirty = true;
        y + line_height
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    pub fn framebuffer_mut(&mut self) -> &mut [u8] {
        &mut self.framebuffer
    }

    pub fn draw_mono_bitmap(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        pixels: &[u8],
    ) {
        for py in 0..height {
            for px in 0..width {
                let index = py * width + px;
                if let Some(&gray) = pixels.get(index) {
                    self.set_pixel(x + px, y + py, gray >= 128);
                }
            }
        }
        self.dirty = true;
    }

    pub fn draw_mono_bitmap_scaled(
        &mut self,
        x: usize,
        y: usize,
        source_size: (usize, usize),
        target_size: (usize, usize),
        pixels: &[u8],
    ) {
        let (source_width, source_height) = source_size;
        let (target_width, target_height) = target_size;
        if source_width == 0 || source_height == 0 || target_width == 0 || target_height == 0 {
            return;
        }

        for py in 0..target_height {
            let source_y = py * source_height / target_height;
            for px in 0..target_width {
                let source_x = px * source_width / target_width;
                let index = source_y * source_width + source_x;
                if let Some(&gray) = pixels.get(index) {
                    self.set_pixel(x + px, y + py, gray >= 128);
                }
            }
        }
        self.dirty = true;
    }

    fn draw_text_run(
        &mut self,
        font: &Font,
        text: &str,
        mut x: usize,
        y: usize,
    ) {
        for ch in text.chars() {
            if ch == ' ' || ch == '\t' {
                x += font.char_advance_width(ch);
                continue;
            }

            self.draw_font_char(font, ch, x, y);
            x += font.char_advance_width(ch);
        }
    }

    fn draw_font_char(
        &mut self,
        font: &Font,
        ch: char,
        x: usize,
        y: usize,
    ) {
        let cp = ch as u32;
        let rendered = {
            let glyph_cache = &mut self.glyph_cache;
            let framebuffer = &mut self.framebuffer;

            if let Some(info) = font.find_glyph(cp) {
                if let Some(bitmap) = glyph_cache.get_or_insert(font, cp, &info) {
                    draw_font_bitmap(framebuffer, font, bitmap, x, y);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };

        if !rendered {
            self.draw_missing_glyph(font, x, y);
        }
    }

    fn draw_char(&mut self, ch: char, x: usize, y: usize, scale: usize) {
        let glyph = glyph_5x7(ch);
        for (col, bits) in glyph.iter().enumerate() {
            for row in 0..7 {
                if (bits >> row) & 1 == 1 {
                    self.fill_rect_pixels(x + col * scale, y + row * scale, scale, scale, false);
                }
            }
        }
    }

    fn fill_rect_pixels(&mut self, x: usize, y: usize, w: usize, h: usize, white: bool) {
        for py in y..(y + h).min(DISPLAY_HEIGHT) {
            for px in x..(x + w).min(DISPLAY_WIDTH) {
                self.set_pixel(px, py, white);
            }
        }
    }

    fn draw_missing_glyph(&mut self, font: &Font, x: usize, y: usize) {
        let width = font.glyph_width as usize;
        let height = font.glyph_height as usize;
        if width < 4 || height < 4 {
            return;
        }

        for dx in 2..width.saturating_sub(2) {
            self.set_pixel(x + dx, y + 2, false);
            self.set_pixel(x + dx, y + height - 3, false);
        }
        for dy in 2..height.saturating_sub(2) {
            self.set_pixel(x + 2, y + dy, false);
            self.set_pixel(x + width - 3, y + dy, false);
        }
    }

    fn set_pixel(&mut self, x: usize, y: usize, white: bool) {
        if x >= DISPLAY_WIDTH || y >= DISPLAY_HEIGHT {
            return;
        }

        let (physical_x, physical_y) = logical_to_physical(x, y);
        let index = physical_y * PHYSICAL_DISPLAY_WIDTH_BYTES + physical_x / 8;
        let mask = 0x80 >> (physical_x % 8);
        if white {
            self.framebuffer[index] |= mask;
        } else {
            self.framebuffer[index] &= !mask;
        }
    }
}

fn logical_to_physical(x: usize, y: usize) -> (usize, usize) {
    debug_assert!(x < DISPLAY_WIDTH);
    debug_assert!(y < DISPLAY_HEIGHT);
    debug_assert_eq!(DISPLAY_WIDTH, PHYSICAL_DISPLAY_HEIGHT);
    debug_assert_eq!(DISPLAY_HEIGHT, PHYSICAL_DISPLAY_WIDTH);

    (y, PHYSICAL_DISPLAY_HEIGHT - 1 - x)
}

fn draw_font_bitmap(fb: &mut [u8], font: &Font, bitmap: &[u8], x: usize, y: usize) {
    for row in 0..font.glyph_height as usize {
        let screen_y = y + row;
        if screen_y >= DISPLAY_HEIGHT {
            break;
        }

        for col in 0..font.glyph_width as usize {
            let screen_x = x + col;
            if screen_x >= DISPLAY_WIDTH {
                break;
            }

            let byte_idx = row * font.row_bytes as usize + col / 8;
            let bit = (bitmap[byte_idx] >> (7 - col % 8)) & 1;
            if bit != 0 {
                set_framebuffer_pixel(fb, screen_x, screen_y, false);
            }
        }
    }
}

fn set_framebuffer_pixel(fb: &mut [u8], x: usize, y: usize, white: bool) {
    let (physical_x, physical_y) = logical_to_physical(x, y);
    let index = physical_y * PHYSICAL_DISPLAY_WIDTH_BYTES + physical_x / 8;
    let mask = 0x80 >> (physical_x % 8);
    if white {
        fb[index] |= mask;
    } else {
        fb[index] &= !mask;
    }
}

fn glyph_5x7(ch: char) -> [u8; 5] {
    match ch {
        'r' => [0x7C, 0x08, 0x04, 0x04, 0x08],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00],
        'a' => [0x38, 0x44, 0x7C, 0x44, 0x44],
        'd' => [0x38, 0x44, 0x44, 0x48, 0x7F],
        'e' => [0x38, 0x54, 0x54, 0x54, 0x18],
        'H' => [0x7F, 0x08, 0x08, 0x08, 0x7F],
        'l' => [0x00, 0x41, 0x7F, 0x40, 0x00],
        'o' => [0x38, 0x44, 0x44, 0x44, 0x38],
        'w' => [0x7C, 0x20, 0x18, 0x20, 0x7C],
        ' ' => [0x00, 0x00, 0x00, 0x00, 0x00],
        _ => [0x7F, 0x41, 0x5D, 0x41, 0x7F],
    }
}
