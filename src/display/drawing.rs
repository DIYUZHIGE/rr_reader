use super::{Display, DISPLAY_HEIGHT, DISPLAY_WIDTH, DISPLAY_WIDTH_BYTES};
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
        let mut decompress_buf = vec![0u8; font.glyph_bytes as usize];
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

            self.draw_font_char(font, ch, cursor_x, cursor_y, &mut decompress_buf);
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
        let mut decompress_buf = vec![0u8; font.glyph_bytes as usize];
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
                self.draw_text_run(font, &unit, cursor_x, y, &mut decompress_buf);
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
                self.draw_font_char(font, word_ch, cursor_x, y, &mut decompress_buf);
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

    fn draw_text_run(
        &mut self,
        font: &Font,
        text: &str,
        mut x: usize,
        y: usize,
        decompress_buf: &mut [u8],
    ) {
        for ch in text.chars() {
            if ch == ' ' || ch == '\t' {
                x += font.char_advance_width(ch);
                continue;
            }

            self.draw_font_char(font, ch, x, y, decompress_buf);
            x += font.char_advance_width(ch);
        }
    }

    fn draw_font_char(
        &mut self,
        font: &Font,
        ch: char,
        x: usize,
        y: usize,
        decompress_buf: &mut [u8],
    ) {
        let cp = ch as u32;
        let rendered = if let Some(info) = font.find_glyph(cp) {
            if let Some(bitmap) = self
                .glyph_cache
                .get_or_insert(font, cp, &info, decompress_buf)
            {
                font.draw_glyph(
                    bitmap,
                    x,
                    y,
                    DISPLAY_WIDTH,
                    DISPLAY_HEIGHT,
                    &mut self.framebuffer,
                );
                true
            } else {
                false
            }
        } else {
            false
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

        let index = y * DISPLAY_WIDTH_BYTES + x / 8;
        let mask = 0x80 >> (x % 8);
        if white {
            self.framebuffer[index] |= mask;
        } else {
            self.framebuffer[index] &= !mask;
        }
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
