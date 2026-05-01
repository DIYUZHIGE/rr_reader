use crate::display::Display;
use crate::font::Font;
use crate::text::is_ascii_word_char;

pub const READER_X: usize = 24;
pub const READER_TEXT_Y: usize = 42;
pub const READER_RIGHT_MARGIN: usize = 24;
pub const READER_BOTTOM_MARGIN: usize = 36;

#[derive(Clone, Copy, Debug)]
pub struct ReaderState {
    pub file_index: usize,
    pub page_index: usize,
}

#[derive(Debug)]
pub struct ReaderCache {
    pub file_index: usize,
    pub file_len: usize,
    pub page_starts: Vec<usize>,
}

pub struct ReaderPaginator<'a> {
    font: &'a Font,
    page_starts: Vec<usize>,
    cursor_x: usize,
    y: usize,
    max_x: usize,
    bottom_y: usize,
    line_height: usize,
    line_width: usize,
    pending_word_start: usize,
    pending_word_end: usize,
    pending_word_width: usize,
    pending_word: String,
}

impl<'a> ReaderPaginator<'a> {
    pub fn new(font: &'a Font) -> Self {
        let max_x = Display::width() - READER_RIGHT_MARGIN;
        Self {
            font,
            page_starts: vec![0],
            cursor_x: READER_X,
            y: READER_TEXT_Y,
            max_x,
            bottom_y: Display::height() - READER_BOTTOM_MARGIN,
            line_height: font.glyph_height as usize + 5,
            line_width: max_x.saturating_sub(READER_X).max(1),
            pending_word_start: 0,
            pending_word_end: 0,
            pending_word_width: 0,
            pending_word: String::with_capacity(32),
        }
    }

    pub fn push_char(&mut self, byte_index: usize, ch: char) {
        if ch == '\r' {
            return;
        }

        if is_ascii_word_char(ch) {
            if self.pending_word.is_empty() {
                self.pending_word_start = byte_index;
            }
            self.pending_word_end = byte_index + ch.len_utf8();
            self.pending_word_width += self.font.char_advance_width(ch);
            self.pending_word.push(ch);
            return;
        }

        self.flush_pending_word();

        if ch == '\n' {
            if !self.advance_line() {
                self.page_starts.push(byte_index + ch.len_utf8());
                self.y = READER_TEXT_Y;
            }
            return;
        }

        let width = self.font.char_advance_width(ch);
        self.process_unit(
            byte_index,
            byte_index + ch.len_utf8(),
            width,
            ch == ' ' || ch == '\t',
        );
    }

    pub fn finish(mut self, file_len: usize) -> Vec<usize> {
        self.flush_pending_word();
        self.page_starts.dedup();
        if self.page_starts.len() > 1 && self.page_starts.last().copied() == Some(file_len) {
            self.page_starts.pop();
        }
        self.page_starts
    }

    fn flush_pending_word(&mut self) {
        if self.pending_word.is_empty() {
            return;
        }

        if self.cursor_x > READER_X
            && self.cursor_x + self.pending_word_width > self.max_x
            && !self.advance_line()
        {
            self.page_starts.push(self.pending_word_start);
            self.y = READER_TEXT_Y;
        }

        if self.pending_word_width <= self.line_width {
            self.cursor_x += self.pending_word_width;
        } else {
            let word_len = self.pending_word.len();
            for offset in 0..word_len {
                let byte = self.pending_word.as_bytes()[offset];
                let absolute_index = self.pending_word_start + offset;
                let width = self.font.char_advance_width(byte as char);
                if self.cursor_x > READER_X
                    && self.cursor_x + width > self.max_x
                    && !self.advance_line()
                {
                    self.page_starts.push(absolute_index);
                    self.y = READER_TEXT_Y;
                }
                self.cursor_x += width;
            }
        }

        self.pending_word.clear();
        self.pending_word_width = 0;
    }

    fn process_unit(&mut self, start: usize, end: usize, width: usize, is_space: bool) {
        if is_space {
            if self.cursor_x == READER_X {
                return;
            }
            if self.cursor_x + width > self.max_x {
                if !self.advance_line() {
                    self.page_starts.push(end);
                    self.y = READER_TEXT_Y;
                }
            } else {
                self.cursor_x += width;
            }
            return;
        }

        if self.cursor_x > READER_X && self.cursor_x + width > self.max_x && !self.advance_line() {
            self.page_starts.push(start);
            self.y = READER_TEXT_Y;
        }
        self.cursor_x += width;
    }

    fn advance_line(&mut self) -> bool {
        self.cursor_x = READER_X;
        if self
            .y
            .saturating_add(self.line_height)
            .saturating_add(self.font.glyph_height as usize)
            > self.bottom_y
        {
            return false;
        }
        self.y += self.line_height;
        true
    }
}
