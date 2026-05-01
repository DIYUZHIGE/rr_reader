use crate::display::Display;
use crate::font::FontSet;
use anyhow::Result;
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

#[derive(Clone, Copy, Debug)]
pub struct ReaderState {
    pub file_index: usize,
    pub page_index: usize,
}

#[derive(Debug)]
pub struct ReaderCache {
    pub file_index: usize,
    pub pages: Vec<ReaderPage>,
}

#[derive(Debug)]
pub struct ReaderPage {
    elements: Vec<PageElement>,
}

#[derive(Debug)]
enum PageElement {
    Line(RenderLine),
    InlineLine(RenderInlineLine),
    Math(RenderMath),
    Image(RenderImage),
}

#[derive(Clone, Debug)]
struct RenderBlock {
    text: String,
    style: BlockStyle,
    indent_level: usize,
    quote_depth: usize,
    prefix: String,
    image: Option<MarkdownImage>,
}

#[derive(Clone, Debug)]
struct RenderLine {
    text: String,
    style: BlockStyle,
    x: usize,
    y: usize,
    quote_depth: usize,
}

#[derive(Debug)]
struct RenderInlineLine {
    style: BlockStyle,
    runs: Vec<RenderInlineRun>,
    x: usize,
    y: usize,
    height: usize,
    quote_depth: usize,
}

#[derive(Debug)]
struct RenderInlineRun {
    x: usize,
    y: usize,
    kind: RenderInlineRunKind,
}

#[derive(Clone, Debug)]
enum RenderInlineRunKind {
    Text(String),
    Math(String),
}

#[derive(Debug)]
struct RenderMath {
    source: String,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    quote_depth: usize,
}

#[derive(Debug)]
struct RenderImage {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    path: String,
    alt: String,
    quote_depth: usize,
}

#[derive(Clone, Debug)]
struct MarkdownImage {
    path: String,
    alt: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockStyle {
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
