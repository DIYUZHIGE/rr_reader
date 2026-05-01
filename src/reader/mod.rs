use crate::display::Display;
use crate::font::FontSet;
use anyhow::Result;
use log::warn;
mod image;
mod markdown;
mod math;
mod pagination;

use self::image::draw_reader_image;
use self::markdown::{parse_markdown_blocks, preprocess_obsidian_embeds};
use self::math::{draw_math_layout, draw_reader_math, layout_math, parse_math};
use self::pagination::{font_for_style, paginate_blocks};
use std::io::{Read, Seek};

pub const READER_X: usize = 24;
pub const READER_TEXT_Y: usize = 42;
pub const READER_RIGHT_MARGIN: usize = 24;
pub const READER_BOTTOM_MARGIN: usize = 36;

const QUOTE_BAR_WIDTH: usize = 3;
const QUOTE_INDENT: usize = 14;
const LIST_INDENT: usize = 18;
const CODE_INDENT: usize = 10;
const IMAGE_TOP_GAP: usize = 6;
const IMAGE_BOTTOM_GAP: usize = 8;
const IMAGE_PLACEHOLDER_HEIGHT: usize = 402;
const INLINE_MATH_START: char = '\u{E000}';
const INLINE_MATH_END: char = '\u{E001}';

/// Number of rendered pages to keep in the sliding window cache.
/// Window covers current_page ± WINDOW_RADIUS pages.
pub const PAGE_CACHE_SIZE: usize = 5;
const WINDOW_RADIUS: usize = PAGE_CACHE_SIZE / 2;

#[derive(Clone, Copy, Debug)]
pub struct ReaderState {
    pub file_index: usize,
    pub page_index: usize,
}

/// Lightweight page cache that only keeps a sliding window of rendered pages.
/// Blocks are retained so pages outside the window can be re-generated on demand
/// without re-reading the SD card or re-parsing markdown.
#[derive(Debug)]
pub struct ReaderCache {
    pub file_index: usize,
    /// Parsed markdown blocks (kept for re-pagination)
    pub blocks: Vec<RenderBlock>,
    /// Total number of pages in this file
    pub page_count: usize,
    /// Sliding window of cached pages: [(page_index, page), ...]
    /// Unused slots contain (0, None).
    pub page_window: [(usize, Option<ReaderPage>); PAGE_CACHE_SIZE],
    /// Starting page index of the current window
    pub window_start: usize,
}

impl ReaderCache {
    /// Ensure the window covers the given page, sliding and re-paginating
    /// if necessary. Call this before `get_page` when the page may have changed.
    pub fn ensure_window(&mut self, page_index: usize, fonts: &FontSet<'_>) {
        if page_index >= self.page_count {
            return;
        }

        // Already in window?
        if self
            .page_window
            .iter()
            .any(|(idx, page)| *idx == page_index && page.is_some())
        {
            return;
        }

        self.slide_window(page_index, fonts);
    }

    /// Get a page by index from the current window. Returns None if the page
    /// is not cached (call `ensure_window` first).
    pub fn get_page(&self, page_index: usize) -> Option<&ReaderPage> {
        self.page_window
            .iter()
            .find(|(idx, page)| *idx == page_index && page.is_some())
            .and_then(|slot| slot.1.as_ref())
    }

    fn slide_window(&mut self, target_page: usize, fonts: &FontSet<'_>) {
        let new_start = target_page
            .saturating_sub(WINDOW_RADIUS)
            .min(self.page_count.saturating_sub(PAGE_CACHE_SIZE));

        // Re-paginate all blocks to get fresh pages, then extract only the window
        let all_pages = paginate_blocks(&self.blocks, fonts);
        let actual_page_count = all_pages.len();
        if actual_page_count != self.page_count {
            warn!(
                "Page count mismatch on re-pagination: {} vs {}",
                actual_page_count, self.page_count
            );
        }

        self.page_window = core::array::from_fn(|_| (0, None));
        let window_end = (new_start + PAGE_CACHE_SIZE).min(all_pages.len());
        for (i, page) in all_pages
            .into_iter()
            .enumerate()
            .skip(new_start)
            .take(window_end.saturating_sub(new_start))
        {
            let slot = i - new_start;
            if slot < PAGE_CACHE_SIZE {
                self.page_window[slot] = (i, Some(page));
            }
        }
        self.window_start = new_start;
    }
}

#[derive(Debug)]
pub struct ReaderPage {
    pub elements: Vec<PageElement>,
}

#[derive(Debug)]
pub enum PageElement {
    Line(RenderLine),
    InlineLine(RenderInlineLine),
    Math(RenderMath),
    Image(RenderImage),
}

#[derive(Clone, Debug)]
pub struct RenderBlock {
    pub text: String,
    pub style: BlockStyle,
    pub indent_level: usize,
    pub quote_depth: usize,
    pub prefix: String,
    pub image: Option<MarkdownImage>,
}

#[derive(Clone, Debug)]
pub struct RenderLine {
    pub text: String,
    pub style: BlockStyle,
    pub x: usize,
    pub y: usize,
    pub quote_depth: usize,
}

#[derive(Debug)]
pub struct RenderInlineLine {
    pub style: BlockStyle,
    pub runs: Vec<RenderInlineRun>,
    pub x: usize,
    pub y: usize,
    pub height: usize,
    pub quote_depth: usize,
}

#[derive(Debug)]
pub struct RenderInlineRun {
    pub x: usize,
    pub y: usize,
    pub kind: RenderInlineRunKind,
}

#[derive(Clone, Debug)]
pub enum RenderInlineRunKind {
    Text(String),
    Math(String),
}

#[derive(Debug)]
pub struct RenderMath {
    pub source: String,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub quote_depth: usize,
}

#[derive(Debug)]
pub struct RenderImage {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub path: String,
    pub alt: String,
    pub quote_depth: usize,
}

#[derive(Clone, Debug)]
pub struct MarkdownImage {
    pub path: String,
    pub alt: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockStyle {
    Paragraph,
    Heading(u8),
    ListItem,
    Quote,
    Code,
    Rule,
    TableRow,
    Image,
    Math,
}

pub fn markdown_pages(markdown: &str, fonts: &FontSet<'_>) -> Vec<ReaderPage> {
    let markdown = preprocess_obsidian_embeds(markdown);
    let blocks = parse_markdown_blocks(&markdown);
    paginate_blocks(&blocks, fonts)
}

/// Parse markdown and return both blocks and all pages.
/// The caller should keep blocks for re-pagination and only cache a window of pages.
pub fn markdown_blocks_and_pages(
    markdown: &str,
    fonts: &FontSet<'_>,
) -> (Vec<RenderBlock>, Vec<ReaderPage>) {
    let markdown = preprocess_obsidian_embeds(markdown);
    let blocks = parse_markdown_blocks(&markdown);
    let pages = paginate_blocks(&blocks, fonts);
    (blocks, pages)
}

pub fn draw_reader_page<F, R>(
    display: &mut Display,
    fonts: &FontSet<'_>,
    page: &ReaderPage,
    mut load_image: F,
) where
    F: FnMut(&str) -> Result<R>,
    R: Read + Seek,
{
    for element in &page.elements {
        match element {
            PageElement::Line(line) => draw_reader_line(display, fonts, line),
            PageElement::InlineLine(line) => draw_reader_inline_line(display, fonts, line),
            PageElement::Math(math) => draw_reader_math(display, fonts, math),
            PageElement::Image(image) => {
                draw_reader_image(display, fonts.ui, image, &mut load_image)
            }
        }
    }
}

fn draw_reader_inline_line(display: &mut Display, fonts: &FontSet<'_>, line: &RenderInlineLine) {
    for depth in 0..line.quote_depth {
        let x = READER_X + depth * QUOTE_INDENT;
        display.fill_rect(x, line.y, QUOTE_BAR_WIDTH, line.height, 0x00);
    }

    let text_font = font_for_style(line.style, fonts);
    for run in &line.runs {
        match &run.kind {
            RenderInlineRunKind::Text(text) => {
                display.draw_text_font(text_font, text, line.x + run.x, line.y + run.y);
            }
            RenderInlineRunKind::Math(source) => {
                let layout = layout_math(&parse_math(source), fonts, false);
                draw_math_layout(display, fonts, &layout, line.x + run.x, line.y + run.y);
            }
        }
    }

    if matches!(line.style, BlockStyle::Heading(1 | 2)) {
        let underline_y = line.y + line.height + 2;
        let width = line
            .runs
            .iter()
            .map(|run| {
                run.x
                    + match &run.kind {
                        RenderInlineRunKind::Text(text) => text_font.text_width(text),
                        RenderInlineRunKind::Math(source) => {
                            layout_math(&parse_math(source), fonts, false).width
                        }
                    }
            })
            .max()
            .unwrap_or(0);
        display.fill_rect(
            line.x,
            underline_y,
            width.min(Display::width().saturating_sub(line.x)),
            1,
            0x00,
        );
    }
}

fn draw_reader_line(display: &mut Display, fonts: &FontSet<'_>, line: &RenderLine) {
    let reader_font = fonts.reader;
    let ui_font = fonts.ui;
    let line_font = font_for_style(line.style, fonts);
    for depth in 0..line.quote_depth {
        let x = READER_X + depth * QUOTE_INDENT;
        display.fill_rect(
            x,
            line.y,
            QUOTE_BAR_WIDTH,
            line_font.glyph_height as usize,
            0x00,
        );
    }

    match line.style {
        BlockStyle::Heading(level) => {
            display.draw_text_font(reader_font, &line.text, line.x, line.y);
            if level <= 2 {
                let underline_y = line.y + reader_font.glyph_height as usize + 2;
                display.fill_rect(
                    line.x,
                    underline_y,
                    reader_font
                        .text_width(&line.text)
                        .min(Display::width().saturating_sub(line.x)),
                    1,
                    0x00,
                );
            }
        }
        BlockStyle::Code => {
            display.fill_rect(
                line.x.saturating_sub(6),
                line.y + 2,
                2,
                (ui_font.glyph_height as usize).saturating_sub(4),
                0x00,
            );
            display.draw_text_font(ui_font, &line.text, line.x, line.y);
        }
        BlockStyle::Rule => {
            display.fill_rect(
                line.x,
                line.y + 3,
                Display::width()
                    .saturating_sub(line.x)
                    .saturating_sub(READER_RIGHT_MARGIN),
                1,
                0x00,
            );
        }
        BlockStyle::Math => {
            display.draw_text_font(fonts.math, &line.text, line.x, line.y);
        }
        _ => {
            display.draw_text_font(reader_font, &line.text, line.x, line.y);
        }
    }
}
