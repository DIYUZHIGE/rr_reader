use crate::display::Display;
use crate::font::{Font, FontSet};
use crate::text::is_ascii_word_char;

use super::math::{layout_math, math_text_baseline, parse_math};
use super::{
    BlockStyle, MarkdownImage, PageElement, ReaderPage, RenderBlock, RenderImage, RenderInlineLine,
    RenderInlineRun, RenderInlineRunKind, RenderLine, RenderMath, CODE_INDENT, IMAGE_BOTTOM_GAP,
    IMAGE_PLACEHOLDER_HEIGHT, IMAGE_TOP_GAP, INLINE_MATH_END, INLINE_MATH_START, LIST_INDENT,
    QUOTE_INDENT, READER_BOTTOM_MARGIN, READER_RIGHT_MARGIN, READER_TEXT_Y, READER_X,
};

pub(super) fn paginate_blocks(blocks: &[RenderBlock], fonts: &FontSet<'_>) -> Vec<ReaderPage> {
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

pub(super) fn font_for_style<'a>(style: BlockStyle, fonts: &FontSet<'a>) -> &'a Font {
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
