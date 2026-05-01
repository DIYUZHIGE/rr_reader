use crate::display::Display;
use crate::font::Font;
use crate::text::is_ascii_word_char;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

pub const READER_X: usize = 24;
pub const READER_TEXT_Y: usize = 42;
pub const READER_RIGHT_MARGIN: usize = 24;
pub const READER_BOTTOM_MARGIN: usize = 36;

const QUOTE_BAR_WIDTH: usize = 3;
const QUOTE_INDENT: usize = 14;
const LIST_INDENT: usize = 18;
const CODE_INDENT: usize = 10;

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
    lines: Vec<RenderLine>,
}

#[derive(Clone, Debug)]
struct RenderBlock {
    text: String,
    style: BlockStyle,
    indent_level: usize,
    quote_depth: usize,
    prefix: String,
}

#[derive(Clone, Debug)]
struct RenderLine {
    text: String,
    style: BlockStyle,
    x: usize,
    y: usize,
    quote_depth: usize,
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
    let blocks = parse_markdown_blocks(markdown);
    paginate_blocks(&blocks, reader_font, ui_font)
}

pub fn draw_reader_page(
    display: &mut Display,
    reader_font: &Font,
    ui_font: &Font,
    page: &ReaderPage,
) {
    for line in &page.lines {
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
}

fn parse_markdown_blocks(markdown: &str) -> Vec<RenderBlock> {
    let mut blocks = Vec::new();
    let mut current: Option<CurrentBlock> = None;
    let mut code_block: Option<String> = None;
    let mut table: Option<TableState> = None;
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
                if let Some(table) = table.as_mut() {
                    table.push_text(text.as_ref());
                } else if let Some(code) = code_block.as_mut() {
                    code.push_str(text.as_ref());
                } else if let Some(block) = current.as_mut() {
                    block.text.push_str(text.as_ref());
                }
            }
            Event::Code(code) => {
                if let Some(table) = table.as_mut() {
                    table.push_text(code.as_ref());
                } else if let Some(block) = current.as_mut() {
                    block.text.push_str(code.as_ref());
                }
            }
            Event::SoftBreak => {
                if let Some(table) = table.as_mut() {
                    table.push_space();
                } else if let Some(block) = current.as_mut() {
                    block.text.push(' ');
                }
            }
            Event::HardBreak => {
                if let Some(table) = table.as_mut() {
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
        });
    }
    if text.ends_with('\n') || text.is_empty() {
        blocks.push(RenderBlock {
            text: String::new(),
            style: BlockStyle::Code,
            indent_level,
            quote_depth,
            prefix: String::new(),
        });
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
    });
}

fn paginate_blocks(blocks: &[RenderBlock], reader_font: &Font, ui_font: &Font) -> Vec<ReaderPage> {
    let mut pages = vec![ReaderPage { lines: Vec::new() }];
    let mut y = READER_TEXT_Y;
    let bottom_y = Display::height() - READER_BOTTOM_MARGIN;

    for block in blocks {
        let font = font_for_style(block.style, reader_font, ui_font);
        let line_step = line_step_for_style(block.style, font);
        let top_gap = top_gap_for_style(block.style);
        let bottom_gap = bottom_gap_for_style(block.style);

        if !block.text.is_empty() || block.style == BlockStyle::Rule {
            y = advance_vertical(&mut pages, y, top_gap, bottom_y, font.glyph_height as usize);
        } else {
            y = advance_vertical(&mut pages, y, 8, bottom_y, font.glyph_height as usize);
            continue;
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
                pages.push(ReaderPage { lines: Vec::new() });
                y = READER_TEXT_Y;
            }

            if let Some(page) = pages.last_mut() {
                page.lines.push(RenderLine {
                    text,
                    style: block.style,
                    x: line_x,
                    y,
                    quote_depth: block.quote_depth,
                });
            }

            y += line_step;
        }

        y = advance_vertical(
            &mut pages,
            y,
            bottom_gap,
            bottom_y,
            font.glyph_height as usize,
        );
    }

    if pages.len() > 1 && pages.last().is_some_and(|page| page.lines.is_empty()) {
        pages.pop();
    }
    if pages.is_empty() {
        pages.push(ReaderPage { lines: Vec::new() });
    }
    pages
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
        pages.push(ReaderPage { lines: Vec::new() });
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
