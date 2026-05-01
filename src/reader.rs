use crate::display::Display;
use crate::font::Font;
use crate::text::is_ascii_word_char;
use anyhow::{anyhow, Result};
use esp_idf_hal::delay::FreeRtos;
use jpeg_decoder::{Decoder as JpegDecoder, PixelFormat};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::io::Cursor;

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
const IMAGE_PLACEHOLDER_HEIGHT: usize = 240;
const JPEG_DECODE_MAX_WIDTH: usize = 128;
const JPEG_DECODE_MAX_HEIGHT: usize = 128;
const MAX_JPEG_FILE_BYTES: usize = 384 * 1024;
const MAX_JPEG_DIMENSION: u16 = 1024;
const MAX_JPEG_DECODE_BUFFER_BYTES: usize = 256 * 1024;

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

#[derive(Debug)]
struct DecodedImage {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
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
}

#[derive(Debug)]
struct ListState {
    next_number: Option<u64>,
}

#[derive(Debug)]
struct ItemState {
    prefix: String,
    has_block: bool,
}

#[derive(Debug)]
struct CurrentBlock {
    text: String,
    style: BlockStyle,
    indent_level: usize,
    quote_depth: usize,
    prefix: String,
}

#[derive(Debug)]
struct ImageState {
    path: String,
    alt: String,
    indent_level: usize,
    quote_depth: usize,
}

#[derive(Debug)]
struct TableState {
    headers: Vec<String>,
    current_row: Vec<String>,
    current_cell: String,
    in_head: bool,
    in_cell: bool,
}

impl TableState {
    fn new() -> Self {
        Self {
            headers: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
            in_head: false,
            in_cell: false,
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.in_cell {
            self.current_cell.push_str(text);
        }
    }

    fn push_space(&mut self) {
        if self.in_cell && !self.current_cell.ends_with(' ') {
            self.current_cell.push(' ');
        }
    }

    fn start_row(&mut self) {
        self.current_row.clear();
    }

    fn start_cell(&mut self) {
        self.current_cell.clear();
        self.in_cell = true;
    }

    fn finish_cell(&mut self) {
        self.current_row
            .push(self.current_cell.trim().replace('\n', " "));
        self.current_cell.clear();
        self.in_cell = false;
    }
}

pub fn markdown_pages(markdown: &str, reader_font: &Font, ui_font: &Font) -> Vec<ReaderPage> {
    let markdown = preprocess_obsidian_embeds(markdown);
    let blocks = parse_markdown_blocks(&markdown);
    paginate_blocks(&blocks, reader_font, ui_font)
}

pub fn draw_reader_page<F>(
    display: &mut Display,
    reader_font: &Font,
    ui_font: &Font,
    page: &ReaderPage,
    mut load_image: F,
) where
    F: FnMut(&str) -> Result<Vec<u8>>,
{
    for element in &page.elements {
        match element {
            PageElement::Line(line) => draw_reader_line(display, reader_font, ui_font, line),
            PageElement::Image(image) => {
                draw_reader_image(display, ui_font, image, &mut load_image)
            }
        }
    }
}

fn draw_reader_line(display: &mut Display, reader_font: &Font, ui_font: &Font, line: &RenderLine) {
    for depth in 0..line.quote_depth {
        let x = READER_X + depth * QUOTE_INDENT;
        display.fill_rect(
            x,
            line.y,
            QUOTE_BAR_WIDTH,
            reader_font.glyph_height as usize,
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
        _ => {
            display.draw_text_font(reader_font, &line.text, line.x, line.y);
        }
    }
}

fn draw_reader_image<F>(
    display: &mut Display,
    ui_font: &Font,
    image: &RenderImage,
    load_image: &mut F,
) where
    F: FnMut(&str) -> Result<Vec<u8>>,
{
    for depth in 0..image.quote_depth {
        let x = READER_X + depth * QUOTE_INDENT;
        display.fill_rect(x, image.y, QUOTE_BAR_WIDTH, image.height, 0x00);
    }

    if is_jpeg_path(&image.path) {
        if let Ok(decoded) = load_image(&image.path)
            .and_then(|bytes| decode_jpeg_to_mono(bytes, image.width, image.height))
        {
            display.draw_mono_bitmap(
                image.x,
                image.y,
                decoded.width,
                decoded.height,
                &decoded.pixels,
            );
            return;
        }
    }

    draw_image_placeholder(display, ui_font, image);
}

fn draw_image_placeholder(display: &mut Display, ui_font: &Font, image: &RenderImage) {
    display.fill_rect(image.x, image.y, image.width, 1, 0x00);
    display.fill_rect(
        image.x,
        image.y + image.height.saturating_sub(1),
        image.width,
        1,
        0x00,
    );
    display.fill_rect(image.x, image.y, 1, image.height, 0x00);
    display.fill_rect(
        image.x + image.width.saturating_sub(1),
        image.y,
        1,
        image.height,
        0x00,
    );

    let label = if image.alt.trim().is_empty() {
        format!("[图片]\n{}", image.path)
    } else {
        format!("[图片] {}\n{}", image.alt, image.path)
    };
    display.draw_text_wrapped(
        ui_font,
        &label,
        image.x + 8,
        image.y + 8,
        image.x + image.width.saturating_sub(8),
        4,
    );
}

fn preprocess_obsidian_embeds(markdown: &str) -> String {
    let mut output = String::with_capacity(markdown.len());
    let mut in_fence = false;

    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");

        if is_fence {
            output.push_str(line);
            in_fence = !in_fence;
            continue;
        }

        if in_fence {
            output.push_str(line);
        } else {
            output.push_str(&preprocess_obsidian_embeds_in_text(line));
        }
    }

    output
}

fn preprocess_obsidian_embeds_in_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("![[") {
        output.push_str(&rest[..start]);

        let after_start = &rest[start + 3..];
        let Some(end) = after_start.find("]]") else {
            output.push_str(&rest[start..]);
            return output;
        };

        let embed = &after_start[..end];
        let image = obsidian_embed_image_state(embed, 0, 0);
        output.push_str("![");
        output.push_str(&escape_markdown_image_alt(&image.alt));
        output.push_str("](<");
        output.push_str(&escape_markdown_destination(&image.path));
        output.push_str(">)");
        rest = &after_start[end + 2..];
    }

    output.push_str(rest);
    output
}

fn escape_markdown_image_alt(alt: &str) -> String {
    alt.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn escape_markdown_destination(path: &str) -> String {
    path.replace('>', "%3E")
}

fn parse_markdown_blocks(markdown: &str) -> Vec<RenderBlock> {
    let mut blocks = Vec::new();
    let mut current: Option<CurrentBlock> = None;
    let mut code_block: Option<String> = None;
    let mut table: Option<TableState> = None;
    let mut image: Option<ImageState> = None;
    let mut list_stack: Vec<ListState> = Vec::new();
    let mut item_stack: Vec<ItemState> = Vec::new();
    let mut quote_depth = 0usize;

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(markdown, options);

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    let mut style = if quote_depth > 0 {
                        BlockStyle::Quote
                    } else {
                        BlockStyle::Paragraph
                    };
                    let mut prefix = String::new();
                    let indent_level = list_stack.len().saturating_sub(1);

                    if let Some(item) = item_stack.last_mut() {
                        if !item.has_block {
                            prefix = item.prefix.clone();
                            item.has_block = true;
                            style = BlockStyle::ListItem;
                        }
                    }

                    current = Some(CurrentBlock {
                        text: String::new(),
                        style,
                        indent_level,
                        quote_depth,
                        prefix,
                    });
                }
                Tag::Heading { level, .. } => {
                    current = Some(CurrentBlock {
                        text: String::new(),
                        style: BlockStyle::Heading(heading_level_number(level)),
                        indent_level: list_stack.len(),
                        quote_depth,
                        prefix: String::new(),
                    });
                }
                Tag::BlockQuote(_) => {
                    flush_current(&mut current, &mut blocks);
                    quote_depth = quote_depth.saturating_add(1);
                }
                Tag::CodeBlock(kind) => {
                    flush_current(&mut current, &mut blocks);
                    let mut text = String::new();
                    if let CodeBlockKind::Fenced(language) = kind {
                        if !language.is_empty() {
                            text.push_str(language.as_ref());
                            text.push('\n');
                        }
                    }
                    code_block = Some(text);
                }
                Tag::Table(_) => {
                    flush_current(&mut current, &mut blocks);
                    table = Some(TableState::new());
                }
                Tag::TableHead => {
                    if let Some(table) = table.as_mut() {
                        table.in_head = true;
                    }
                }
                Tag::TableRow => {
                    if let Some(table) = table.as_mut() {
                        table.start_row();
                    }
                }
                Tag::TableCell => {
                    if let Some(table) = table.as_mut() {
                        table.start_cell();
                    }
                }
                Tag::Image { dest_url, .. } => {
                    flush_current(&mut current, &mut blocks);
                    image = Some(ImageState {
                        path: dest_url.to_string(),
                        alt: String::new(),
                        indent_level: list_stack.len(),
                        quote_depth,
                    });
                }
                Tag::List(first) => {
                    list_stack.push(ListState { next_number: first });
                }
                Tag::Item => {
                    let prefix = if let Some(list) = list_stack.last_mut() {
                        if let Some(number) = list.next_number.as_mut() {
                            let prefix = format!("{}. ", *number);
                            *number = number.saturating_add(1);
                            prefix
                        } else {
                            "- ".to_owned()
                        }
                    } else {
                        "- ".to_owned()
                    };
                    item_stack.push(ItemState {
                        prefix,
                        has_block: false,
                    });
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) => {
                    flush_current(&mut current, &mut blocks);
                }
                TagEnd::BlockQuote(_) => {
                    flush_current(&mut current, &mut blocks);
                    quote_depth = quote_depth.saturating_sub(1);
                    push_blank(&mut blocks);
                }
                TagEnd::CodeBlock => {
                    if let Some(text) = code_block.take() {
                        push_code_blocks(&mut blocks, text, list_stack.len(), quote_depth);
                    }
                }
                TagEnd::Table => {
                    table = None;
                    push_blank(&mut blocks);
                }
                TagEnd::TableHead => {
                    if let Some(table) = table.as_mut() {
                        table.in_head = false;
                    }
                }
                TagEnd::TableRow => {
                    if let Some(table) = table.as_mut() {
                        if table.in_head {
                            table.headers = table.current_row.clone();
                        } else {
                            push_table_row(
                                &mut blocks,
                                &table.headers,
                                &table.current_row,
                                list_stack.len(),
                                quote_depth,
                            );
                        }
                    }
                }
                TagEnd::TableCell => {
                    if let Some(table) = table.as_mut() {
                        table.finish_cell();
                    }
                }
                TagEnd::Image => {
                    if let Some(image) = image.take() {
                        push_image_block(&mut blocks, image);
                    }
                }
                TagEnd::List(_) => {
                    list_stack.pop();
                    push_blank(&mut blocks);
                }
                TagEnd::Item => {
                    flush_current(&mut current, &mut blocks);
                    item_stack.pop();
                }
                _ => {}
            },
            Event::Text(text) => {
                if let Some(image) = image.as_mut() {
                    image.alt.push_str(text.as_ref());
                } else if let Some(table) = table.as_mut() {
                    table.push_text(text.as_ref());
                } else if let Some(code) = code_block.as_mut() {
                    code.push_str(text.as_ref());
                } else {
                    push_text_with_obsidian_embeds(
                        text.as_ref(),
                        &mut current,
                        &mut blocks,
                        &mut item_stack,
                        &list_stack,
                        quote_depth,
                    );
                }
            }
            Event::Code(code) => {
                if let Some(image) = image.as_mut() {
                    image.alt.push_str(code.as_ref());
                } else if let Some(table) = table.as_mut() {
                    table.push_text(code.as_ref());
                } else if let Some(block) = current.as_mut() {
                    block.text.push_str(code.as_ref());
                } else {
                    current = Some(default_current_block(
                        &mut item_stack,
                        &list_stack,
                        quote_depth,
                    ));
                    if let Some(block) = current.as_mut() {
                        block.text.push_str(code.as_ref());
                    }
                }
            }
            Event::SoftBreak => {
                if let Some(image) = image.as_mut() {
                    if !image.alt.ends_with(' ') {
                        image.alt.push(' ');
                    }
                } else if let Some(table) = table.as_mut() {
                    table.push_space();
                } else if let Some(block) = current.as_mut() {
                    block.text.push(' ');
                }
            }
            Event::HardBreak => {
                if let Some(image) = image.as_mut() {
                    if !image.alt.ends_with(' ') {
                        image.alt.push(' ');
                    }
                } else if let Some(table) = table.as_mut() {
                    table.push_space();
                } else if let Some(block) = current.as_mut() {
                    block.text.push('\n');
                }
            }
            Event::Rule => {
                flush_current(&mut current, &mut blocks);
                blocks.push(RenderBlock {
                    text: String::new(),
                    style: BlockStyle::Rule,
                    indent_level: list_stack.len(),
                    quote_depth,
                    prefix: String::new(),
                    image: None,
                });
            }
            Event::TaskListMarker(checked) => {
                if let Some(block) = current.as_mut() {
                    block.text.push_str(if checked { "[x] " } else { "[ ] " });
                }
            }
            _ => {}
        }
    }

    flush_current(&mut current, &mut blocks);
    blocks
}

fn push_code_blocks(
    blocks: &mut Vec<RenderBlock>,
    text: String,
    indent_level: usize,
    quote_depth: usize,
) {
    for line in text.lines() {
        blocks.push(RenderBlock {
            text: line.to_owned(),
            style: BlockStyle::Code,
            indent_level,
            quote_depth,
            prefix: String::new(),
            image: None,
        });
    }
    if text.ends_with('\n') || text.is_empty() {
        blocks.push(RenderBlock {
            text: String::new(),
            style: BlockStyle::Code,
            indent_level,
            quote_depth,
            prefix: String::new(),
            image: None,
        });
    }
}

fn push_image_block(blocks: &mut Vec<RenderBlock>, image: ImageState) {
    blocks.push(RenderBlock {
        text: image.alt.clone(),
        style: BlockStyle::Image,
        indent_level: image.indent_level,
        quote_depth: image.quote_depth,
        prefix: String::new(),
        image: Some(MarkdownImage {
            path: image.path,
            alt: image.alt,
        }),
    });
}

fn push_text_with_obsidian_embeds(
    text: &str,
    current: &mut Option<CurrentBlock>,
    blocks: &mut Vec<RenderBlock>,
    item_stack: &mut [ItemState],
    list_stack: &[ListState],
    quote_depth: usize,
) {
    let mut rest = text;

    while let Some(start) = rest.find("![[") {
        let before = &rest[..start];
        append_current_text(before, current, item_stack, list_stack, quote_depth);

        let after_start = &rest[start + 3..];
        let Some(end) = after_start.find("]]") else {
            append_current_text(&rest[start..], current, item_stack, list_stack, quote_depth);
            return;
        };

        flush_current(current, blocks);
        let embed = &after_start[..end];
        push_image_block(
            blocks,
            obsidian_embed_image_state(embed, list_stack.len(), quote_depth),
        );
        rest = &after_start[end + 2..];
    }

    append_current_text(rest, current, item_stack, list_stack, quote_depth);
}

fn append_current_text(
    text: &str,
    current: &mut Option<CurrentBlock>,
    item_stack: &mut [ItemState],
    list_stack: &[ListState],
    quote_depth: usize,
) {
    if text.is_empty() {
        return;
    }

    if current.is_none() {
        *current = Some(default_current_block(item_stack, list_stack, quote_depth));
    }

    if let Some(block) = current.as_mut() {
        block.text.push_str(text);
    }
}

fn obsidian_embed_image_state(embed: &str, indent_level: usize, quote_depth: usize) -> ImageState {
    let (path, label) = embed
        .split_once('|')
        .map(|(path, label)| (path.trim(), label.trim()))
        .unwrap_or_else(|| (embed.trim(), ""));
    let alt = if label.is_empty() || looks_like_image_size(label) {
        image_alt_from_path(path)
    } else {
        label.to_owned()
    };

    ImageState {
        path: path.to_owned(),
        alt,
        indent_level,
        quote_depth,
    }
}

fn looks_like_image_size(label: &str) -> bool {
    let label = label.trim();
    !label.is_empty()
        && label
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, 'x' | 'X'))
        && label.chars().any(|ch| ch.is_ascii_digit())
}

fn image_alt_from_path(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .split_once('#')
        .map(|(path, _)| path)
        .unwrap_or_else(|| path.rsplit(['/', '\\']).next().unwrap_or(path))
        .to_owned()
}

fn default_current_block(
    item_stack: &mut [ItemState],
    list_stack: &[ListState],
    quote_depth: usize,
) -> CurrentBlock {
    let mut style = if quote_depth > 0 {
        BlockStyle::Quote
    } else {
        BlockStyle::Paragraph
    };
    let mut prefix = String::new();
    let indent_level = list_stack.len().saturating_sub(1);

    if let Some(item) = item_stack.last_mut() {
        if !item.has_block {
            prefix = item.prefix.clone();
            item.has_block = true;
            style = BlockStyle::ListItem;
        }
    }

    CurrentBlock {
        text: String::new(),
        style,
        indent_level,
        quote_depth,
        prefix,
    }
}

fn push_table_row(
    blocks: &mut Vec<RenderBlock>,
    headers: &[String],
    row: &[String],
    indent_level: usize,
    quote_depth: usize,
) {
    if row.iter().all(|cell| cell.trim().is_empty()) {
        return;
    }

    let mut text = String::new();
    for (index, value) in row.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        let header = table_header(headers, index);
        text.push_str(&header);
        text.push_str(": ");
        text.push_str(value.trim());
    }

    blocks.push(RenderBlock {
        text,
        style: BlockStyle::TableRow,
        indent_level,
        quote_depth,
        prefix: String::new(),
        image: None,
    });
}

fn table_header(headers: &[String], index: usize) -> String {
    headers
        .get(index)
        .map(|header| header.trim())
        .filter(|header| !header.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Column {}", index + 1))
}

fn flush_current(current: &mut Option<CurrentBlock>, blocks: &mut Vec<RenderBlock>) {
    let Some(block) = current.take() else {
        return;
    };

    let text = block.text.trim_end_matches('\n').to_owned();
    if text.trim().is_empty() && block.prefix.is_empty() {
        return;
    }

    blocks.push(RenderBlock {
        text,
        style: block.style,
        indent_level: block.indent_level,
        quote_depth: block.quote_depth,
        prefix: block.prefix,
        image: None,
    });
}

fn push_blank(blocks: &mut Vec<RenderBlock>) {
    if matches!(blocks.last(), Some(block) if block.text.is_empty() && block.style == BlockStyle::Paragraph)
    {
        return;
    }

    blocks.push(RenderBlock {
        text: String::new(),
        style: BlockStyle::Paragraph,
        indent_level: 0,
        quote_depth: 0,
        prefix: String::new(),
        image: None,
    });
}

fn paginate_blocks(blocks: &[RenderBlock], reader_font: &Font, ui_font: &Font) -> Vec<ReaderPage> {
    let mut pages = vec![ReaderPage {
        elements: Vec::new(),
    }];
    let mut y = READER_TEXT_Y;
    let bottom_y = Display::height() - READER_BOTTOM_MARGIN;

    for block in blocks {
        if let Some(image) = block.image.as_ref() {
            y = paginate_image_block(&mut pages, y, bottom_y, block, image);
        } else {
            y = paginate_text_block(&mut pages, y, bottom_y, block, reader_font, ui_font);
        }
    }

    if pages.len() > 1 && pages.last().is_some_and(|page| page.elements.is_empty()) {
        pages.pop();
    }
    if pages.is_empty() {
        pages.push(ReaderPage {
            elements: Vec::new(),
        });
    }
    pages
}

fn paginate_text_block(
    pages: &mut Vec<ReaderPage>,
    mut y: usize,
    bottom_y: usize,
    block: &RenderBlock,
    reader_font: &Font,
    ui_font: &Font,
) -> usize {
    let font = font_for_style(block.style, reader_font, ui_font);
    let line_step = line_step_for_style(block.style, font);
    let top_gap = top_gap_for_style(block.style);
    let bottom_gap = bottom_gap_for_style(block.style);

    if !block.text.is_empty() || block.style == BlockStyle::Rule {
        y = advance_vertical(pages, y, top_gap, bottom_y, font.glyph_height as usize);
    } else {
        return advance_vertical(pages, y, 8, bottom_y, font.glyph_height as usize);
    }

    let x = block_x(block);
    let first_x = x;
    let continuation_x = (x + font.text_width(&block.prefix)).min(max_content_x());
    let wrapped = if block.style == BlockStyle::Rule {
        vec![(String::new(), x)]
    } else {
        wrap_block_text(font, block, first_x, continuation_x)
    };

    for (text, line_x) in wrapped {
        if y + font.glyph_height as usize > bottom_y {
            pages.push(ReaderPage {
                elements: Vec::new(),
            });
            y = READER_TEXT_Y;
        }

        if let Some(page) = pages.last_mut() {
            page.elements.push(PageElement::Line(RenderLine {
                text,
                style: block.style,
                x: line_x,
                y,
                quote_depth: block.quote_depth,
            }));
        }

        y += line_step;
    }

    advance_vertical(pages, y, bottom_gap, bottom_y, font.glyph_height as usize)
}

fn paginate_image_block(
    pages: &mut Vec<ReaderPage>,
    mut y: usize,
    bottom_y: usize,
    block: &RenderBlock,
    image: &MarkdownImage,
) -> usize {
    let x = block_x(block);
    let width = max_content_x().saturating_sub(x).max(1);
    let height = IMAGE_PLACEHOLDER_HEIGHT.min(bottom_y.saturating_sub(READER_TEXT_Y).max(1));

    y = advance_vertical(pages, y, IMAGE_TOP_GAP, bottom_y, height);
    if y + height > bottom_y {
        pages.push(ReaderPage {
            elements: Vec::new(),
        });
        y = READER_TEXT_Y;
    }

    if let Some(page) = pages.last_mut() {
        page.elements.push(PageElement::Image(RenderImage {
            x,
            y,
            width,
            height,
            path: image.path.clone(),
            alt: image.alt.clone(),
            quote_depth: block.quote_depth,
        }));
    }

    advance_vertical(pages, y + height, IMAGE_BOTTOM_GAP, bottom_y, 1)
}

fn is_jpeg_path(path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

fn decode_jpeg_to_mono(
    bytes: Vec<u8>,
    max_width: usize,
    max_height: usize,
) -> Result<DecodedImage> {
    if bytes.len() > MAX_JPEG_FILE_BYTES {
        return Err(anyhow!(
            "jpeg file too large: {} > {} bytes",
            bytes.len(),
            MAX_JPEG_FILE_BYTES
        ));
    }

    FreeRtos::delay_ms(1);
    let mut decoder = JpegDecoder::new(Cursor::new(bytes));
    decoder.set_max_decoding_buffer_size(MAX_JPEG_DECODE_BUFFER_BYTES);
    decoder
        .read_info()
        .map_err(|e| anyhow!("read jpeg info: {}", e))?;
    let info = decoder.info().ok_or_else(|| anyhow!("jpeg info missing"))?;
    if info.width > MAX_JPEG_DIMENSION || info.height > MAX_JPEG_DIMENSION {
        return Err(anyhow!(
            "jpeg dimensions too large: {}x{}",
            info.width,
            info.height
        ));
    }

    let decode_max_width = max_width.min(JPEG_DECODE_MAX_WIDTH).max(1);
    let decode_max_height = max_height.min(JPEG_DECODE_MAX_HEIGHT).max(1);
    let requested =
        jpeg_scale_request(info.width, info.height, decode_max_width, decode_max_height);
    let (scaled_width, scaled_height) = decoder
        .scale(requested.0, requested.1)
        .map_err(|e| anyhow!("scale jpeg: {}", e))?;
    if usize::from(scaled_width)
        .saturating_mul(usize::from(scaled_height))
        .saturating_mul(3)
        > MAX_JPEG_DECODE_BUFFER_BYTES
    {
        return Err(anyhow!(
            "scaled jpeg buffer too large: {}x{}",
            scaled_width,
            scaled_height
        ));
    }

    FreeRtos::delay_ms(1);
    let decoded = decoder
        .decode()
        .map_err(|e| anyhow!("decode jpeg: {}", e))?;
    FreeRtos::delay_ms(1);
    let info = decoder
        .info()
        .ok_or_else(|| anyhow!("decoded jpeg info missing"))?;
    let source_width = usize::from(info.width);
    let source_height = usize::from(info.height);
    let source_channels = info.pixel_format.pixel_bytes();
    if source_width == 0 || source_height == 0 || source_channels == 0 {
        return Err(anyhow!("invalid jpeg output"));
    }

    let (target_width, target_height) = fit_dimensions(
        source_width,
        source_height,
        decode_max_width,
        decode_max_height,
    );
    let pixels = jpeg_to_mono_nearest(
        &decoded,
        source_width,
        source_height,
        source_channels,
        info.pixel_format,
        target_width,
        target_height,
    )?;

    Ok(DecodedImage {
        width: target_width,
        height: target_height,
        pixels,
    })
}

fn jpeg_scale_request(
    source_width: u16,
    source_height: u16,
    max_width: usize,
    max_height: usize,
) -> (u16, u16) {
    let max_width = max_width.min(u16::MAX as usize).max(1) as u16;
    let max_height = max_height.min(u16::MAX as usize).max(1) as u16;
    let width_ratio = source_width.div_ceil(max_width).max(1);
    let height_ratio = source_height.div_ceil(max_height).max(1);
    let ratio = width_ratio.max(height_ratio);
    let scale = if ratio <= 1 {
        1
    } else if ratio <= 2 {
        2
    } else if ratio <= 4 {
        4
    } else {
        8
    };

    (
        (source_width / scale).max(1),
        (source_height / scale).max(1),
    )
}

fn fit_dimensions(
    source_width: usize,
    source_height: usize,
    max_width: usize,
    max_height: usize,
) -> (usize, usize) {
    if source_width <= max_width && source_height <= max_height {
        return (source_width.max(1), source_height.max(1));
    }

    let width_limited_height = source_height.saturating_mul(max_width) / source_width.max(1);
    if width_limited_height <= max_height {
        (max_width.max(1), width_limited_height.max(1))
    } else {
        let width = source_width.saturating_mul(max_height) / source_height.max(1);
        (width.max(1), max_height.max(1))
    }
}

fn jpeg_to_mono_nearest(
    decoded: &[u8],
    source_width: usize,
    source_height: usize,
    source_channels: usize,
    pixel_format: PixelFormat,
    target_width: usize,
    target_height: usize,
) -> Result<Vec<u8>> {
    let mut mono = vec![0xFF; target_width * target_height];

    for y in 0..target_height {
        let source_y = y * source_height / target_height;
        for x in 0..target_width {
            let source_x = x * source_width / target_width;
            let source_index = (source_y * source_width + source_x) * source_channels;
            let gray = jpeg_gray_at(decoded, source_index, pixel_format)?;
            mono[y * target_width + x] = if gray < 150 { 0x00 } else { 0xFF };
        }
        if y % 24 == 0 {
            FreeRtos::delay_ms(1);
        }
    }

    Ok(mono)
}

fn jpeg_gray_at(decoded: &[u8], index: usize, pixel_format: PixelFormat) -> Result<u8> {
    match pixel_format {
        PixelFormat::L8 => decoded
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("jpeg luma pixel out of range")),
        PixelFormat::RGB24 => {
            let r = *decoded
                .get(index)
                .ok_or_else(|| anyhow!("jpeg red pixel out of range"))? as u16;
            let g = *decoded
                .get(index + 1)
                .ok_or_else(|| anyhow!("jpeg green pixel out of range"))?
                as u16;
            let b = *decoded
                .get(index + 2)
                .ok_or_else(|| anyhow!("jpeg blue pixel out of range"))? as u16;
            Ok(((r * 77 + g * 150 + b * 29) >> 8) as u8)
        }
        PixelFormat::CMYK32 => {
            let c = *decoded
                .get(index)
                .ok_or_else(|| anyhow!("jpeg cyan pixel out of range"))? as u16;
            let m = *decoded
                .get(index + 1)
                .ok_or_else(|| anyhow!("jpeg magenta pixel out of range"))?
                as u16;
            let y = *decoded
                .get(index + 2)
                .ok_or_else(|| anyhow!("jpeg yellow pixel out of range"))?
                as u16;
            let k = *decoded
                .get(index + 3)
                .ok_or_else(|| anyhow!("jpeg black pixel out of range"))?
                as u16;
            let r = 255u16.saturating_sub((c + k).min(255));
            let g = 255u16.saturating_sub((m + k).min(255));
            let b = 255u16.saturating_sub((y + k).min(255));
            Ok(((r * 77 + g * 150 + b * 29) >> 8) as u8)
        }
        PixelFormat::L16 => decoded
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("jpeg l16 pixel out of range")),
    }
}

fn wrap_block_text(
    font: &Font,
    block: &RenderBlock,
    first_x: usize,
    continuation_x: usize,
) -> Vec<(String, usize)> {
    let mut lines = Vec::new();
    let mut first_segment = true;

    for segment in block.text.split('\n') {
        let text = if first_segment {
            format!("{}{}", block.prefix, segment)
        } else {
            segment.to_owned()
        };
        let x = if first_segment {
            first_x
        } else {
            continuation_x
        };
        wrap_text_segment(font, &text, x, continuation_x, &mut lines);
        first_segment = false;
    }

    if lines.is_empty() {
        lines.push((block.prefix.clone(), first_x));
    }

    lines
}

fn wrap_text_segment(
    font: &Font,
    text: &str,
    first_x: usize,
    continuation_x: usize,
    lines: &mut Vec<(String, usize)>,
) {
    let max_x = Display::width() - READER_RIGHT_MARGIN;
    let mut current_x = first_x;
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut iter = text.chars().peekable();

    while let Some(ch) = iter.next() {
        let mut unit = String::new();
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
        let is_space = unit.chars().all(|c| c == ' ' || c == '\t');
        if is_space && current.is_empty() {
            continue;
        }

        if !current.is_empty() && current_x + current_width + unit_width > max_x {
            lines.push((current, current_x));
            current = String::new();
            current_x = continuation_x;
            current_width = 0;
            if is_space {
                continue;
            }
        }

        let available_width = max_x.saturating_sub(current_x).max(1);
        if unit_width > available_width {
            if !current.is_empty() {
                lines.push((current, current_x));
                current = String::new();
                current_x = continuation_x;
                current_width = 0;
            }
            split_long_unit(font, &unit, continuation_x, lines);
            continue;
        }

        current_width += unit_width;
        current.push_str(&unit);
    }

    if !current.is_empty() {
        lines.push((current, current_x));
    }
}

fn split_long_unit(
    font: &Font,
    unit: &str,
    continuation_x: usize,
    lines: &mut Vec<(String, usize)>,
) {
    let max_x = Display::width() - READER_RIGHT_MARGIN;
    let mut current = String::new();
    let mut width = 0usize;

    for ch in unit.chars() {
        let advance = font.char_advance_width(ch);
        if !current.is_empty() && continuation_x + width + advance > max_x {
            lines.push((current, continuation_x));
            current = String::new();
            width = 0;
        }
        current.push(ch);
        width += advance;
    }

    if !current.is_empty() {
        lines.push((current, continuation_x));
    }
}

fn advance_vertical(
    pages: &mut Vec<ReaderPage>,
    y: usize,
    amount: usize,
    bottom_y: usize,
    glyph_height: usize,
) -> usize {
    if amount == 0 {
        return y;
    }
    if y != READER_TEXT_Y && y + amount + glyph_height > bottom_y {
        pages.push(ReaderPage {
            elements: Vec::new(),
        });
        READER_TEXT_Y
    } else {
        y + amount
    }
}

fn block_x(block: &RenderBlock) -> usize {
    let quote_offset = block.quote_depth * QUOTE_INDENT;
    let list_offset = block.indent_level * LIST_INDENT;
    let style_offset = if block.style == BlockStyle::Code {
        CODE_INDENT
    } else {
        0
    };
    (READER_X + quote_offset + list_offset + style_offset).min(max_content_x())
}

fn max_content_x() -> usize {
    Display::width()
        .saturating_sub(READER_RIGHT_MARGIN)
        .saturating_sub(1)
}

fn font_for_style<'a>(style: BlockStyle, reader_font: &'a Font, ui_font: &'a Font) -> &'a Font {
    match style {
        BlockStyle::Code => ui_font,
        _ => reader_font,
    }
}

fn line_step_for_style(style: BlockStyle, font: &Font) -> usize {
    match style {
        BlockStyle::Heading(_) => font.glyph_height as usize + 8,
        BlockStyle::Code => font.glyph_height as usize + 3,
        BlockStyle::Rule => 10,
        BlockStyle::TableRow => font.glyph_height as usize + 4,
        _ => font.glyph_height as usize + 5,
    }
}

fn top_gap_for_style(style: BlockStyle) -> usize {
    match style {
        BlockStyle::Heading(1) => 8,
        BlockStyle::Heading(_) => 6,
        BlockStyle::Code => 4,
        BlockStyle::Rule => 6,
        BlockStyle::TableRow => 4,
        _ => 0,
    }
}

fn bottom_gap_for_style(style: BlockStyle) -> usize {
    match style {
        BlockStyle::Heading(_) => 6,
        BlockStyle::Code => 4,
        BlockStyle::Rule => 6,
        BlockStyle::TableRow => 6,
        _ => 2,
    }
}

fn heading_level_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}
