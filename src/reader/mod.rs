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
pub use self::pagination::PaginationCursor;
use self::pagination::{font_for_style, paginate_blocks, paginate_window_from_cursor};

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

/// Max number of rendered pages to keep in the sliding window cache.
/// Actual window size is selected dynamically based on free heap.
pub const PAGE_CACHE_SIZE_MAX: usize = 5;
pub const PAGE_CACHE_SIZE_MIN: usize = 3;

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
    /// Optional full rendered pages cache (enabled when heap budget allows).
    /// If present, window slides reuse these pages instead of re-paginating.
    pub all_pages: Option<Vec<ReaderPage>>,
    /// Sliding window of cached pages: [(page_index, page), ...]
    pub page_window: Vec<(usize, Option<ReaderPage>)>,
    /// Active number of slots in `page_window`
    pub window_len: usize,
    /// Cursor used by cursor-based pagination path.
    pub window_cursor: PaginationCursor,
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
            .take(self.window_len)
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
        let window_radius = self.window_len / 2;
        let new_start = target_page
            .saturating_sub(window_radius)
            .min(self.page_count.saturating_sub(self.window_len));

        self.page_window.clear();
        self.page_window
            .resize_with(self.window_len.max(1), || (0, None));

        if let Some(all_pages) = self.all_pages.as_ref() {
            let window_end = (new_start + self.window_len).min(all_pages.len());
            for i in new_start..window_end {
                let slot = i - new_start;
                if slot < self.window_len {
                    self.page_window[slot] = (i, Some(all_pages[i].clone()));
                }
            }
            return;
        }

        // Fallback: use cursor-based window pagination (temporary adapter currently
        // backed by full pagination internally). This keeps the cache API window-centric
        // as we migrate to true incremental pagination.
        let cursor =
            if self.window_cursor.block_index > 0 && self.window_cursor.page_index == new_start {
                self.window_cursor
            } else {
                PaginationCursor {
                    block_index: 0,
                    page_index: new_start,
                    y: READER_TEXT_Y,
                }
            };
        let (next_cursor, window, actual_page_count) = paginate_window_from_cursor(
            &self.blocks,
            fonts,
            &cursor,
            self.window_len.max(1),
            Some(self.page_count),
        );
        if actual_page_count != self.page_count {
            warn!(
                "Page count mismatch on re-pagination: {} vs {}",
                actual_page_count, self.page_count
            );
            self.page_count = actual_page_count;
        }

        for (slot, (idx, page)) in window.into_iter().enumerate() {
            if slot < self.window_len {
                self.page_window[slot] = (idx, Some(page));
            }
        }
        self.window_cursor = next_cursor;
    }
}

#[derive(Clone, Debug)]
pub struct ReaderPage {
    pub elements: Vec<PageElement>,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
pub struct RenderInlineLine {
    pub style: BlockStyle,
    pub runs: Vec<RenderInlineRun>,
    pub x: usize,
    pub y: usize,
    pub height: usize,
    pub quote_depth: usize,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
pub struct RenderMath {
    pub source: String,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub quote_depth: usize,
}

#[derive(Clone, Debug)]
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

pub fn draw_reader_page<F>(
    display: &mut Display,
    fonts: &FontSet<'_>,
    page: &ReaderPage,
    mut resolve_image_path: F,
) where
    F: FnMut(&str) -> Result<String>,
{
    for element in &page.elements {
        match element {
            PageElement::Line(line) => draw_reader_line(display, fonts, line),
            PageElement::InlineLine(line) => draw_reader_inline_line(display, fonts, line),
            PageElement::Math(math) => draw_reader_math(display, fonts, math),
            PageElement::Image(image) => {
                draw_reader_image(display, fonts.ui, image, &mut resolve_image_path)
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
