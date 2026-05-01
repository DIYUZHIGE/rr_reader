use crate::display::Display;
use crate::font::{Font, FontSet};
use crate::text::is_ascii_word_char;
use anyhow::Result;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

mod image;
mod math;

use self::image::draw_reader_image;
use self::math::{draw_math_layout, draw_reader_math, layout_math, math_text_baseline, parse_math};
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
    options.insert(Options::ENABLE_MATH);
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
            Event::InlineMath(math) => {
                let text = encode_inline_math(math.as_ref());
                append_current_text(
                    &text,
                    &mut current,
                    &mut item_stack,
                    &list_stack,
                    quote_depth,
                );
            }
            Event::DisplayMath(math) => {
                flush_current(&mut current, &mut blocks);
                push_math_block(&mut blocks, math.as_ref(), list_stack.len(), quote_depth);
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

fn push_math_block(
    blocks: &mut Vec<RenderBlock>,
    text: &str,
    indent_level: usize,
    quote_depth: usize,
) {
    for line in text.lines() {
        blocks.push(RenderBlock {
            text: line.to_owned(),
            style: BlockStyle::Math,
            indent_level,
            quote_depth,
            prefix: String::new(),
            image: None,
        });
    }
    if text.ends_with('\n') || text.is_empty() {
        blocks.push(RenderBlock {
            text: String::new(),
            style: BlockStyle::Math,
            indent_level,
            quote_depth,
            prefix: String::new(),
            image: None,
        });
    }
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

fn encode_inline_math(source: &str) -> String {
    let mut output = String::with_capacity(source.len() + 2);
    output.push(INLINE_MATH_START);
    output.push_str(source);
    output.push(INLINE_MATH_END);
    output
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

fn paginate_blocks(blocks: &[RenderBlock], fonts: &FontSet<'_>) -> Vec<ReaderPage> {
    let mut pages = vec![ReaderPage {
        elements: Vec::new(),
    }];
    let mut y = READER_TEXT_Y;
    let bottom_y = Display::height() - READER_BOTTOM_MARGIN;

    for block in blocks {
        if let Some(image) = block.image.as_ref() {
            y = paginate_image_block(&mut pages, y, bottom_y, block, image);
        } else if block.style == BlockStyle::Math {
            y = paginate_math_block(&mut pages, y, bottom_y, block, fonts);
        } else {
            y = paginate_text_block(&mut pages, y, bottom_y, block, fonts);
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
    fonts: &FontSet<'_>,
) -> usize {
    let font = font_for_style(block.style, fonts);
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

    if block.text.contains(INLINE_MATH_START) {
        return paginate_inline_text_block(
            pages,
            y,
            bottom_y,
            block,
            fonts,
            first_x,
            continuation_x,
            top_gap,
            bottom_gap,
        );
    }

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

fn paginate_math_block(
    pages: &mut Vec<ReaderPage>,
    mut y: usize,
    bottom_y: usize,
    block: &RenderBlock,
    fonts: &FontSet<'_>,
) -> usize {
    let layout = layout_math(&parse_math(&block.text), fonts, false);
    let top_gap = top_gap_for_style(BlockStyle::Math);
    let bottom_gap = bottom_gap_for_style(BlockStyle::Math);
    let height = layout.height.max(fonts.math.glyph_height as usize);
    let x = block_x(block);

    y = advance_vertical(pages, y, top_gap, bottom_y, height);
    if y + height > bottom_y {
        pages.push(ReaderPage {
            elements: Vec::new(),
        });
        y = READER_TEXT_Y;
    }

    if let Some(page) = pages.last_mut() {
        page.elements.push(PageElement::Math(RenderMath {
            source: block.text.clone(),
            x,
            y,
            width: layout.width,
            height,
            quote_depth: block.quote_depth,
        }));
    }

    advance_vertical(
        pages,
        y + height,
        bottom_gap,
        bottom_y,
        fonts.math.glyph_height as usize,
    )
}

#[derive(Debug)]
struct InlineLineSpec {
    x: usize,
    height: usize,
    runs: Vec<InlineRunSpec>,
}

#[derive(Debug)]
struct InlineRunSpec {
    x: usize,
    y: usize,
    kind: RenderInlineRunKind,
}

#[derive(Clone, Debug)]
struct InlineUnit {
    width: usize,
    height: usize,
    baseline: usize,
    kind: RenderInlineRunKind,
}

fn paginate_inline_text_block(
    pages: &mut Vec<ReaderPage>,
    mut y: usize,
    bottom_y: usize,
    block: &RenderBlock,
    fonts: &FontSet<'_>,
    first_x: usize,
    continuation_x: usize,
    top_gap: usize,
    bottom_gap: usize,
) -> usize {
    let font = font_for_style(block.style, fonts);
    let min_height = font.glyph_height as usize;
    let line_step_extra =
        line_step_for_style(block.style, font).saturating_sub(font.glyph_height as usize);
    let line_specs = wrap_inline_block_text(fonts, font, block, first_x, continuation_x);

    y = advance_vertical(pages, y, top_gap, bottom_y, min_height);
    for spec in line_specs {
        if y + spec.height > bottom_y {
            pages.push(ReaderPage {
                elements: Vec::new(),
            });
            y = READER_TEXT_Y;
        }

        let runs = spec
            .runs
            .into_iter()
            .map(|run| RenderInlineRun {
                x: run.x,
                y: run.y,
                kind: run.kind,
            })
            .collect();

        if let Some(page) = pages.last_mut() {
            page.elements
                .push(PageElement::InlineLine(RenderInlineLine {
                    style: block.style,
                    runs,
                    x: spec.x,
                    y,
                    height: spec.height,
                    quote_depth: block.quote_depth,
                }));
        }

        y += spec.height + line_step_extra;
    }

    advance_vertical(pages, y, bottom_gap, bottom_y, min_height)
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

fn wrap_inline_block_text(
    fonts: &FontSet<'_>,
    text_font: &Font,
    block: &RenderBlock,
    first_x: usize,
    continuation_x: usize,
) -> Vec<InlineLineSpec> {
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
        let units = inline_units(fonts, text_font, &text);
        wrap_inline_units(text_font, units, x, continuation_x, &mut lines);
        first_segment = false;
    }

    if lines.is_empty() {
        let units = inline_units(fonts, text_font, &block.prefix);
        push_inline_line(text_font, first_x, units, &mut lines);
    }

    lines
}

fn inline_units(fonts: &FontSet<'_>, text_font: &Font, text: &str) -> Vec<InlineUnit> {
    let mut units = Vec::new();
    let mut plain = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == INLINE_MATH_START {
            flush_inline_text_units(text_font, &mut plain, &mut units);
            let mut source = String::new();
            for next in chars.by_ref() {
                if next == INLINE_MATH_END {
                    break;
                }
                source.push(next);
            }
            let layout = layout_math(&parse_math(&source), fonts, false);
            units.push(InlineUnit {
                width: layout.width,
                height: layout.height,
                baseline: layout.baseline,
                kind: RenderInlineRunKind::Math(source),
            });
        } else {
            plain.push(ch);
        }
    }

    flush_inline_text_units(text_font, &mut plain, &mut units);
    units
}

fn flush_inline_text_units(font: &Font, text: &mut String, units: &mut Vec<InlineUnit>) {
    if text.is_empty() {
        return;
    }

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
        push_text_inline_unit(font, unit, units);
    }

    text.clear();
}

fn push_text_inline_unit(font: &Font, text: String, units: &mut Vec<InlineUnit>) {
    units.push(InlineUnit {
        width: font.text_width(&text),
        height: font.glyph_height as usize,
        baseline: math_text_baseline(font),
        kind: RenderInlineRunKind::Text(text),
    });
}

fn wrap_inline_units(
    text_font: &Font,
    units: Vec<InlineUnit>,
    first_x: usize,
    continuation_x: usize,
    lines: &mut Vec<InlineLineSpec>,
) {
    let max_x = Display::width() - READER_RIGHT_MARGIN;
    let mut current_x = first_x;
    let mut current = Vec::new();
    let mut current_width = 0usize;

    for unit in units {
        let is_space = inline_unit_is_space(&unit);
        if is_space && current.is_empty() {
            continue;
        }

        if !current.is_empty() && current_x + current_width + unit.width > max_x {
            push_inline_line(text_font, current_x, std::mem::take(&mut current), lines);
            current_x = continuation_x;
            current_width = 0;
            if is_space {
                continue;
            }
        }

        let available_width = max_x.saturating_sub(current_x).max(1);
        if unit.width > available_width {
            if !current.is_empty() {
                push_inline_line(text_font, current_x, std::mem::take(&mut current), lines);
                current_x = continuation_x;
                current_width = 0;
            }
            if let RenderInlineRunKind::Text(text) = &unit.kind {
                split_inline_text_unit(text_font, text, continuation_x, lines);
                continue;
            }
        }

        current_width += unit.width;
        current.push(unit);
    }

    if !current.is_empty() {
        push_inline_line(text_font, current_x, current, lines);
    }
}

fn split_inline_text_unit(
    text_font: &Font,
    text: &str,
    continuation_x: usize,
    lines: &mut Vec<InlineLineSpec>,
) {
    let max_x = Display::width() - READER_RIGHT_MARGIN;
    let mut current = Vec::new();
    let mut width = 0usize;

    for ch in text.chars() {
        let unit_text = ch.to_string();
        let unit = InlineUnit {
            width: text_font.text_width(&unit_text),
            height: text_font.glyph_height as usize,
            baseline: math_text_baseline(text_font),
            kind: RenderInlineRunKind::Text(unit_text),
        };
        if !current.is_empty() && continuation_x + width + unit.width > max_x {
            push_inline_line(
                text_font,
                continuation_x,
                std::mem::take(&mut current),
                lines,
            );
            width = 0;
        }
        width += unit.width;
        current.push(unit);
    }

    if !current.is_empty() {
        push_inline_line(text_font, continuation_x, current, lines);
    }
}

fn push_inline_line(
    text_font: &Font,
    x: usize,
    units: Vec<InlineUnit>,
    lines: &mut Vec<InlineLineSpec>,
) {
    let baseline = units
        .iter()
        .map(|unit| unit.baseline)
        .max()
        .unwrap_or_else(|| math_text_baseline(text_font));
    let below = units
        .iter()
        .map(|unit| unit.height.saturating_sub(unit.baseline))
        .max()
        .unwrap_or_else(|| text_font.glyph_height as usize - math_text_baseline(text_font));
    let height = (baseline + below).max(text_font.glyph_height as usize);

    let mut run_x = 0usize;
    let mut runs = Vec::with_capacity(units.len());
    for unit in units {
        let y = baseline.saturating_sub(unit.baseline);
        let width = unit.width;
        runs.push(InlineRunSpec {
            x: run_x,
            y,
            kind: unit.kind,
        });
        run_x += width;
    }

    lines.push(InlineLineSpec { x, height, runs });
}

fn inline_unit_is_space(unit: &InlineUnit) -> bool {
    matches!(&unit.kind, RenderInlineRunKind::Text(text) if text.chars().all(|ch| ch == ' ' || ch == '\t'))
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

fn font_for_style<'a>(style: BlockStyle, fonts: &FontSet<'a>) -> &'a Font {
    match style {
        BlockStyle::Code => fonts.ui,
        BlockStyle::Math => fonts.math,
        _ => fonts.reader,
    }
}

fn line_step_for_style(style: BlockStyle, font: &Font) -> usize {
    match style {
        BlockStyle::Heading(_) => font.glyph_height as usize + 8,
        BlockStyle::Code => font.glyph_height as usize + 3,
        BlockStyle::Math => font.glyph_height as usize + 5,
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
        BlockStyle::Math => 6,
        BlockStyle::Rule => 6,
        BlockStyle::TableRow => 4,
        _ => 0,
    }
}

fn bottom_gap_for_style(style: BlockStyle) -> usize {
    match style {
        BlockStyle::Heading(_) => 6,
        BlockStyle::Code => 4,
        BlockStyle::Math => 6,
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
