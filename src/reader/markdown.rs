use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use super::{BlockStyle, MarkdownImage, RenderBlock, WikiLink, INLINE_MATH_END, INLINE_MATH_START};

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

pub fn preprocess_obsidian_embeds(markdown: &str) -> String {
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

pub fn parse_markdown_blocks(markdown: &str) -> Vec<RenderBlock> {
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
                    wiki_links: Vec::new(),
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

    // Extract wiki links from each block's text
    for block in &mut blocks {
        if !matches!(
            block.style,
            BlockStyle::Code | BlockStyle::Math | BlockStyle::Rule | BlockStyle::Image
        ) {
            let (new_text, links) = extract_wiki_links(&block.text);
            block.text = new_text;
            block.wiki_links = links;
        }
    }

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
            wiki_links: Vec::new(),
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
            wiki_links: Vec::new(),
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
            wiki_links: Vec::new(),
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
            wiki_links: Vec::new(),
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
        wiki_links: Vec::new(),
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
        wiki_links: Vec::new(),
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
        wiki_links: Vec::new(),
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
        wiki_links: Vec::new(),
    });
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

/// Extract Obsidian wiki links from text, replacing `[[target]]` with the
/// display alias and returning link metadata for navigation.
pub(super) fn extract_wiki_links(text: &str) -> (String, Vec<WikiLink>) {
    if !text.contains("[[") {
        return (text.to_owned(), Vec::new());
    }

    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut links = Vec::new();
    let mut pos = 0usize;

    while pos < bytes.len() {
        // Check for `![[` (image embed passthrough)
        if bytes[pos] == b'!'
            && pos + 2 < bytes.len()
            && bytes[pos + 1] == b'['
            && bytes[pos + 2] == b'['
        {
            out.push('!');
            out.push_str("[[");
            pos += 3;
            // Find closing `]]`, copying chars properly
            while pos < bytes.len() {
                if bytes[pos] == b']' && pos + 1 < bytes.len() && bytes[pos + 1] == b']' {
                    out.push_str("]]");
                    pos += 2;
                    break;
                }
                let ch = next_utf8_char(text, &mut pos);
                out.push(ch);
            }
            continue;
        }

        // Check for `[[` (wiki link)
        if bytes[pos] == b'[' && pos + 1 < bytes.len() && bytes[pos + 1] == b'[' {
            pos += 2;
            let link_start = pos;
            // Find closing `]]`
            while pos < bytes.len() {
                if bytes[pos] == b']' && pos + 1 < bytes.len() && bytes[pos + 1] == b']' {
                    break;
                }
                pos += 1;
            }
            if pos >= bytes.len() || bytes[pos] != b']' {
                // No closing ]], treat as literal text
                out.push_str("[[");
                continue;
            }
            let link_end = pos;
            pos += 2; // skip `]]`

            let inner = &text[link_start..link_end];
            let (target, alias) = match inner.split_once('|') {
                Some((target, alias)) => (target.trim().to_owned(), alias.trim().to_owned()),
                None => {
                    let t = inner.trim().to_owned();
                    (t.clone(), t)
                }
            };
            let start_byte = out.len();
            out.push_str(&alias);
            let end_byte = out.len();
            links.push(WikiLink {
                target,
                alias,
                start_byte,
                end_byte,
            });
        } else {
            let ch = next_utf8_char(text, &mut pos);
            out.push(ch);
        }
    }

    (out, links)
}

/// Read the next UTF-8 character from `text` starting at `*pos`,
/// advancing `pos` by the character's byte length.
fn next_utf8_char(text: &str, pos: &mut usize) -> char {
    let tail = &text[*pos..];
    let ch = tail.chars().next().unwrap_or('\u{FFFD}');
    *pos += ch.len_utf8();
    ch
}
