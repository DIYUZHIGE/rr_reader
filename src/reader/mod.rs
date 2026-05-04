use crate::display::Display;
use crate::font::FontSet;
use anyhow::{anyhow, Result};
use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
mod image;
pub mod markdown;
mod math;
mod pagination;

use self::image::draw_reader_image;
pub(super) use self::markdown::parse_markdown_blocks;
pub use self::markdown::preprocess_obsidian_embeds;
use self::math::{draw_math_layout, draw_reader_math, layout_math, parse_math};
use self::pagination::{font_for_style, paginate_blocks};

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

/// Minimal page cache backed by SD card files.
#[derive(Debug)]
pub struct ReaderCache {
    pub file_index: usize,
    /// Total number of pages
    pub page_count: usize,
    /// Path to the cache directory on SD
    pub cache_dir: String,
    /// Currently loaded page in memory (index, page)
    pub current: Option<(usize, ReaderPage)>,
}

const CACHE_ROOT: &str = "/sdcard/vault/.rr_cache";

fn file_hash(path: &str) -> u64 {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

fn cache_path(file_path: &str) -> String {
    format!("{}/{:016x}.pages", CACHE_ROOT, file_hash(file_path))
}

impl ReaderCache {
    /// Load page from SD cache, re-parsing and writing cache if needed.
    pub fn load(file_index: usize, md_path: &str, fonts: &FontSet<'_>) -> Result<Self> {
        let cpath = cache_path(md_path);
        // Try reading cached pages
        if let Ok(cache) = Self::from_cache(file_index, &cpath) {
            return Ok(cache);
        }
        // Parse and cache to SD
        Self::build_cache(file_index, md_path, &cpath, fonts)
    }

    fn from_cache(file_index: usize, path: &str) -> Result<Self> {
        let mut f = File::open(path)?;
        let mut buf = [0u8; 8];
        f.read_exact(&mut buf)?;
        let page_count = u64::from_le_bytes(buf) as usize;
        Ok(Self {
            file_index,
            page_count,
            cache_dir: path.to_string(),
            current: None,
        })
    }

    fn build_cache(
        file_index: usize,
        md_path: &str,
        cpath: &str,
        fonts: &FontSet<'_>,
    ) -> Result<Self> {
        let markdown = fs::read_to_string(md_path)?;
        let all_pages = markdown_blocks_and_pages(markdown, fonts);
        let page_count = all_pages.len();

        // Create cache dir and write pages
        if let Some(parent) = std::path::Path::new(cpath).parent() {
            fs::create_dir_all(parent).ok();
        }
        let mut f = File::create(cpath)?;
        f.write_all(&(page_count as u64).to_le_bytes())?;
        for page in &all_pages {
            let data = page.to_bytes();
            f.write_all(&(data.len() as u32).to_le_bytes())?;
            f.write_all(&data)?;
        }
        drop(f);
        drop(all_pages);

        Ok(Self {
            file_index,
            page_count,
            cache_dir: cpath.to_string(),
            current: None,
        })
    }

    pub fn get_page(&mut self, page_index: usize) -> Option<&ReaderPage> {
        if page_index >= self.page_count {
            return None;
        }
        if let Some((idx, _)) = self.current {
            if idx == page_index {
                return self.current.as_ref().map(|(_, p)| p);
            }
        }
        // Read one page from SD
        match self.read_page_from_sd(page_index) {
            Ok(page) => {
                self.current = Some((page_index, page));
                self.current.as_ref().map(|(_, p)| p)
            }
            Err(_) => None,
        }
    }

    fn read_page_from_sd(&self, page_index: usize) -> Result<ReaderPage> {
        let mut f = File::open(&self.cache_dir)?;
        let mut buf = [0u8; 8];
        f.read_exact(&mut buf)?;
        let count = u64::from_le_bytes(buf) as usize;
        if page_index >= count {
            return Err(anyhow!("page index out of range"));
        }
        // Skip pages before target
        for _ in 0..page_index {
            let mut len_buf = [0u8; 4];
            f.read_exact(&mut len_buf)?;
            let len = u32::from_le_bytes(len_buf) as usize;
            // seek forward
            use std::io::Seek;
            f.seek(std::io::SeekFrom::Current(len as i64))?;
        }
        let mut len_buf = [0u8; 4];
        f.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut data = vec![0u8; len];
        f.read_exact(&mut data)?;
        ReaderPage::from_bytes(&data)
    }

    pub fn load_page(&mut self, page_index: usize) -> Option<&ReaderPage> {
        self.get_page(page_index)
    }
}

#[derive(Clone, Debug)]
pub struct ReaderPage {
    pub elements: Vec<PageElement>,
}

impl ReaderPage {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(&(self.elements.len() as u16).to_le_bytes());
        for el in &self.elements {
            el.write_to(&mut out);
        }
        out
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < 2 {
            return Err(anyhow!("page data too short"));
        }
        let count = u16::from_le_bytes([data[0], data[1]]) as usize;
        let mut pos = 2;
        let mut elements = Vec::with_capacity(count);
        for _ in 0..count {
            let (el, used) = PageElement::read_from(&data[pos..])?;
            pos += used;
            elements.push(el);
        }
        Ok(Self { elements })
    }
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

// --- Binary serialization helpers ---

fn write_u16(out: &mut Vec<u8>, v: u16) {
    out.extend(&v.to_le_bytes());
}
fn read_u16(data: &[u8], pos: &mut usize) -> u16 {
    let v = u16::from_le_bytes([data[*pos], data[*pos + 1]]);
    *pos += 2;
    v
}
fn write_str(out: &mut Vec<u8>, s: &str) {
    write_u16(out, s.len() as u16);
    out.extend(s.as_bytes());
}
fn read_str(data: &[u8], pos: &mut usize) -> String {
    let len = read_u16(data, pos) as usize;
    let s = String::from_utf8_lossy(&data[*pos..*pos + len]).into_owned();
    *pos += len;
    s
}

impl PageElement {
    fn write_to(&self, out: &mut Vec<u8>) {
        match self {
            PageElement::Line(l) => {
                out.push(0);
                write_str(out, &l.text);
                out.push(l.style.tag());
                out.push(l.style.extra());
                write_u16(out, l.x as u16);
                write_u16(out, l.y as u16);
                out.push(l.quote_depth as u8);
            }
            PageElement::InlineLine(l) => {
                out.push(1);
                out.push(l.style.tag());
                out.push(l.style.extra());
                write_u16(out, l.x as u16);
                write_u16(out, l.y as u16);
                write_u16(out, l.height as u16);
                out.push(l.quote_depth as u8);
                write_u16(out, l.runs.len() as u16);
                for r in &l.runs {
                    write_u16(out, r.x as u16);
                    write_u16(out, r.y as u16);
                    match &r.kind {
                        RenderInlineRunKind::Text(t) => {
                            out.push(0);
                            write_str(out, t);
                        }
                        RenderInlineRunKind::Math(m) => {
                            out.push(1);
                            write_str(out, m);
                        }
                    }
                }
            }
            PageElement::Math(m) => {
                out.push(2);
                write_str(out, &m.source);
                write_u16(out, m.x as u16);
                write_u16(out, m.y as u16);
                write_u16(out, m.height as u16);
                out.push(m.quote_depth as u8);
            }
            PageElement::Image(img) => {
                out.push(3);
                write_u16(out, img.x as u16);
                write_u16(out, img.y as u16);
                write_u16(out, img.width as u16);
                write_u16(out, img.height as u16);
                write_str(out, &img.path);
                write_str(out, &img.alt);
                out.push(img.quote_depth as u8);
            }
        }
    }

    fn read_from(data: &[u8]) -> Result<(Self, usize)> {
        if data.is_empty() {
            return Err(anyhow!("empty element data"));
        }
        let mut pos = 1;
        let el = match data[0] {
            0 => {
                let text = read_str(data, &mut pos);
                let tag = data[pos];
                pos += 1;
                let extra = data[pos];
                pos += 1;
                let x = read_u16(data, &mut pos) as usize;
                let y = read_u16(data, &mut pos) as usize;
                let qd = data[pos] as usize;
                pos += 1;
                PageElement::Line(RenderLine {
                    text,
                    style: BlockStyle::from_tag(tag, extra),
                    x,
                    y,
                    quote_depth: qd,
                })
            }
            1 => {
                let tag = data[pos];
                pos += 1;
                let extra = data[pos];
                pos += 1;
                let x = read_u16(data, &mut pos) as usize;
                let y = read_u16(data, &mut pos) as usize;
                let height = read_u16(data, &mut pos) as usize;
                let qd = data[pos] as usize;
                pos += 1;
                let run_count = read_u16(data, &mut pos) as usize;
                let mut runs = Vec::with_capacity(run_count);
                for _ in 0..run_count {
                    let rx = read_u16(data, &mut pos) as usize;
                    let ry = read_u16(data, &mut pos) as usize;
                    let kind = {
                        let tag = data[pos];
                        pos += 1;
                        match tag {
                            0 => RenderInlineRunKind::Text(read_str(data, &mut pos)),
                            _ => RenderInlineRunKind::Math(read_str(data, &mut pos)),
                        }
                    };
                    runs.push(RenderInlineRun { x: rx, y: ry, kind });
                }
                PageElement::InlineLine(RenderInlineLine {
                    style: BlockStyle::from_tag(tag, extra),
                    runs,
                    x,
                    y,
                    height,
                    quote_depth: qd,
                })
            }
            2 => {
                let source = read_str(data, &mut pos);
                let x = read_u16(data, &mut pos) as usize;
                let y = read_u16(data, &mut pos) as usize;
                let height = read_u16(data, &mut pos) as usize;
                let qd = data[pos] as usize;
                pos += 1;
                PageElement::Math(RenderMath {
                    source,
                    x,
                    y,
                    height,
                    quote_depth: qd,
                })
            }
            3 => {
                let x = read_u16(data, &mut pos) as usize;
                let y = read_u16(data, &mut pos) as usize;
                let width = read_u16(data, &mut pos) as usize;
                let height = read_u16(data, &mut pos) as usize;
                let path = read_str(data, &mut pos);
                let alt = read_str(data, &mut pos);
                let qd = data[pos] as usize;
                pos += 1;
                PageElement::Image(RenderImage {
                    x,
                    y,
                    width,
                    height,
                    path,
                    alt,
                    quote_depth: qd,
                })
            }
            _ => return Err(anyhow!("unknown element tag")),
        };
        Ok((el, pos))
    }
}

impl BlockStyle {
    fn tag(&self) -> u8 {
        match self {
            BlockStyle::Paragraph => 0,
            BlockStyle::Heading(_) => 1,
            BlockStyle::ListItem => 2,
            BlockStyle::Quote => 3,
            BlockStyle::Code => 4,
            BlockStyle::Rule => 5,
            BlockStyle::TableRow => 6,
            BlockStyle::Image => 7,
            BlockStyle::Math => 8,
        }
    }
    fn extra(&self) -> u8 {
        match self {
            BlockStyle::Heading(l) => *l,
            _ => 0,
        }
    }
    fn from_tag(tag: u8, extra: u8) -> Self {
        match tag {
            1 => BlockStyle::Heading(extra),
            2 => BlockStyle::ListItem,
            3 => BlockStyle::Quote,
            4 => BlockStyle::Code,
            5 => BlockStyle::Rule,
            6 => BlockStyle::TableRow,
            7 => BlockStyle::Image,
            8 => BlockStyle::Math,
            _ => BlockStyle::Paragraph,
        }
    }
}

/// Parse markdown and return all pages.
/// Avoids preprocessing copy when no Obsidian embeds are present.
pub fn markdown_blocks_and_pages(markdown: String, fonts: &FontSet<'_>) -> Vec<ReaderPage> {
    // Only preprocess if Obsidian embed syntax is present.
    let needs_preprocess = markdown.contains("![[");
    if needs_preprocess {
        let preprocessed = preprocess_obsidian_embeds(&markdown);
        drop(markdown);
        let blocks = parse_markdown_blocks(&preprocessed);
        let pages = paginate_blocks(&blocks, fonts);
        drop(blocks);
        pages
    } else {
        let blocks = parse_markdown_blocks(&markdown);
        drop(markdown);
        let pages = paginate_blocks(&blocks, fonts);
        drop(blocks);
        pages
    }
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
