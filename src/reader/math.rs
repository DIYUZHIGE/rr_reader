use crate::display::Display;
use crate::font::{Font, FontSet};

use super::{RenderMath, QUOTE_BAR_WIDTH, QUOTE_INDENT, READER_X};

const MAX_MATH_NESTING_DEPTH: usize = 32;

#[derive(Clone, Debug)]
pub(super) enum MathNode {
    Row(Vec<MathNode>),
    Text(String),
    SupSub {
        base: Box<MathNode>,
        sup: Option<Box<MathNode>>,
        sub: Option<Box<MathNode>>,
    },
    Fraction {
        numerator: Box<MathNode>,
        denominator: Box<MathNode>,
    },
    Sqrt(Box<MathNode>),
}

#[derive(Clone, Copy, Debug)]
enum MathFontRole {
    Math,
    Script,
}

#[derive(Debug)]
struct PositionedMathBox {
    x: usize,
    y: usize,
    item: MathLayout,
}

#[derive(Debug)]
enum MathLayoutKind {
    Text(String, MathFontRole),
    Row(Vec<PositionedMathBox>),
    Fraction {
        numerator: Box<PositionedMathBox>,
        denominator: Box<PositionedMathBox>,
        bar_y: usize,
    },
    Sqrt {
        body: Box<PositionedMathBox>,
        overbar_x: usize,
        overbar_y: usize,
        overbar_width: usize,
    },
}

#[derive(Debug)]
pub(super) struct MathLayout {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) baseline: usize,
    kind: MathLayoutKind,
}

pub(super) fn draw_reader_math(display: &mut Display, fonts: &FontSet<'_>, math: &RenderMath) {
    for depth in 0..math.quote_depth {
        let x = READER_X + depth * QUOTE_INDENT;
        display.fill_rect(x, math.y, QUOTE_BAR_WIDTH, math.height, 0x00);
    }

    let layout = layout_math(&parse_math(&math.source), fonts, false);
    draw_math_layout(display, fonts, &layout, math.x, math.y);
}

pub(super) fn draw_math_layout(
    display: &mut Display,
    fonts: &FontSet<'_>,
    layout: &MathLayout,
    x: usize,
    y: usize,
) {
    match &layout.kind {
        MathLayoutKind::Text(text, role) => {
            display.draw_text_font(math_font_for_role(fonts, *role), text, x, y);
        }
        MathLayoutKind::Row(items) => {
            for item in items {
                draw_math_layout(display, fonts, &item.item, x + item.x, y + item.y);
            }
        }
        MathLayoutKind::Fraction {
            numerator,
            denominator,
            bar_y,
        } => {
            draw_math_layout(
                display,
                fonts,
                &numerator.item,
                x + numerator.x,
                y + numerator.y,
            );
            display.fill_rect(x, y + *bar_y, layout.width, 1, 0x00);
            draw_math_layout(
                display,
                fonts,
                &denominator.item,
                x + denominator.x,
                y + denominator.y,
            );
        }
        MathLayoutKind::Sqrt {
            body,
            overbar_x,
            overbar_y,
            overbar_width,
        } => {
            display.draw_text_font(
                fonts.math,
                "\u{221a}",
                x,
                y + layout
                    .baseline
                    .saturating_sub(fonts.math.glyph_height as usize * 3 / 4),
            );
            display.fill_rect(x + *overbar_x, y + *overbar_y, *overbar_width, 1, 0x00);
            draw_math_layout(display, fonts, &body.item, x + body.x, y + body.y);
        }
    }
}

pub(super) fn parse_math(source: &str) -> MathNode {
    MathParser::new(source).parse_row(false, 0)
}

struct MathParser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> MathParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            chars: source.chars().peekable(),
        }
    }

    fn parse_row(&mut self, stop_on_group: bool, depth: usize) -> MathNode {
        if depth > MAX_MATH_NESTING_DEPTH {
            self.skip_to_group_end(stop_on_group);
            return MathNode::Text("\u{2026}".to_owned());
        }

        let mut nodes = Vec::new();
        let mut text = String::new();

        while let Some(&ch) = self.chars.peek() {
            if stop_on_group && ch == '}' {
                self.chars.next();
                break;
            }

            match ch {
                '^' | '_' => {
                    self.chars.next();
                    flush_math_text(&mut text, &mut nodes);
                    let script = self.parse_script_argument(depth + 1);
                    let base = nodes.pop().unwrap_or_else(|| MathNode::Text(String::new()));
                    nodes.push(merge_script(base, ch == '^', script));
                }
                '{' => {
                    self.chars.next();
                    flush_math_text(&mut text, &mut nodes);
                    nodes.push(self.parse_row(true, depth + 1));
                }
                '\\' => {
                    self.chars.next();
                    flush_math_text(&mut text, &mut nodes);
                    nodes.push(self.parse_command(depth + 1));
                }
                _ => {
                    self.chars.next();
                    text.push(ch);
                }
            }
        }

        flush_math_text(&mut text, &mut nodes);
        if nodes.len() == 1 {
            nodes
                .pop()
                .expect("math parser invariant: len == 1 must have one node")
        } else {
            MathNode::Row(nodes)
        }
    }

    /// Consume chars until the current group ends or end-of-input.
    fn skip_to_group_end(&mut self, stop_on_group: bool) {
        let mut brace_depth = 0usize;
        while let Some(ch) = self.chars.next() {
            match ch {
                '{' => brace_depth += 1,
                '}' if stop_on_group && brace_depth == 0 => break,
                '}' => brace_depth = brace_depth.saturating_sub(1),
                _ => {}
            }
        }
    }

    fn parse_script_argument(&mut self, depth: usize) -> MathNode {
        if depth > MAX_MATH_NESTING_DEPTH {
            return MathNode::Text("\u{2026}".to_owned());
        }
        match self.chars.peek().copied() {
            Some('{') => {
                self.chars.next();
                self.parse_row(true, depth + 1)
            }
            Some('\\') => {
                self.chars.next();
                self.parse_command(depth + 1)
            }
            Some(ch) => {
                self.chars.next();
                MathNode::Text(ch.to_string())
            }
            None => MathNode::Text(String::new()),
        }
    }

    fn parse_required_group(&mut self, depth: usize) -> MathNode {
        if depth > MAX_MATH_NESTING_DEPTH {
            return MathNode::Text("\u{2026}".to_owned());
        }
        consume_math_spaces(&mut self.chars);
        if matches!(self.chars.peek(), Some('{')) {
            self.chars.next();
            self.parse_row(true, depth + 1)
        } else {
            self.parse_script_argument(depth)
        }
    }

    fn parse_command(&mut self, depth: usize) -> MathNode {
        if depth > MAX_MATH_NESTING_DEPTH {
            return MathNode::Text("\u{2026}".to_owned());
        }
        let mut command = String::new();
        while let Some(&next) = self.chars.peek() {
            if next.is_ascii_alphabetic() {
                command.push(next);
                self.chars.next();
            } else {
                break;
            }
        }

        if command.is_empty() {
            return self
                .chars
                .next()
                .map(|ch| MathNode::Text(ch.to_string()))
                .unwrap_or_else(|| MathNode::Text("\\".to_owned()));
        }

        match command.as_str() {
            "frac" => {
                let numerator = self.parse_required_group(depth + 1);
                let denominator = self.parse_required_group(depth + 1);
                MathNode::Fraction {
                    numerator: Box::new(numerator),
                    denominator: Box::new(denominator),
                }
            }
            "sqrt" => {
                skip_optional_math_group(&mut self.chars);
                MathNode::Sqrt(Box::new(self.parse_required_group(depth + 1)))
            }
            "left" | "right" => {
                consume_math_spaces(&mut self.chars);
                match self.chars.next() {
                    Some('.') => MathNode::Text(String::new()),
                    Some(ch) => MathNode::Text(ch.to_string()),
                    None => MathNode::Text(String::new()),
                }
            }
            _ => MathNode::Text(
                tex_symbol(&command)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("\\{}", command)),
            ),
        }
    }
}

fn flush_math_text(text: &mut String, nodes: &mut Vec<MathNode>) {
    if !text.is_empty() {
        nodes.push(MathNode::Text(std::mem::take(text)));
    }
}

fn merge_script(base: MathNode, is_sup: bool, script: MathNode) -> MathNode {
    match base {
        MathNode::SupSub { base, sup, sub } => {
            if is_sup {
                MathNode::SupSub {
                    base,
                    sup: Some(Box::new(script)),
                    sub,
                }
            } else {
                MathNode::SupSub {
                    base,
                    sup,
                    sub: Some(Box::new(script)),
                }
            }
        }
        base => {
            if is_sup {
                MathNode::SupSub {
                    base: Box::new(base),
                    sup: Some(Box::new(script)),
                    sub: None,
                }
            } else {
                MathNode::SupSub {
                    base: Box::new(base),
                    sup: None,
                    sub: Some(Box::new(script)),
                }
            }
        }
    }
}

fn consume_math_spaces(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while matches!(chars.peek(), Some(ch) if ch.is_whitespace()) {
        chars.next();
    }
}

fn skip_optional_math_group(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    consume_math_spaces(chars);
    if !matches!(chars.peek(), Some('[')) {
        return;
    }

    chars.next();
    let mut depth = 1usize;
    for ch in chars.by_ref() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
}

pub(super) fn layout_math(node: &MathNode, fonts: &FontSet<'_>, script: bool) -> MathLayout {
    match node {
        MathNode::Row(nodes) => layout_math_row(nodes, fonts, script),
        MathNode::Text(text) => layout_math_text(text, fonts, script),
        MathNode::SupSub { base, sup, sub } => layout_math_supsub(base, sup, sub, fonts, script),
        MathNode::Fraction {
            numerator,
            denominator,
        } => layout_math_fraction(numerator, denominator, fonts),
        MathNode::Sqrt(body) => layout_math_sqrt(body, fonts),
    }
}

fn layout_math_text(text: &str, fonts: &FontSet<'_>, script: bool) -> MathLayout {
    let role = if script {
        MathFontRole::Script
    } else {
        MathFontRole::Math
    };
    let font = math_font_for_role(fonts, role);
    MathLayout {
        width: font.text_width(text),
        height: font.glyph_height as usize,
        baseline: math_text_baseline(font),
        kind: MathLayoutKind::Text(text.to_owned(), role),
    }
}

fn layout_math_row(nodes: &[MathNode], fonts: &FontSet<'_>, script: bool) -> MathLayout {
    if nodes.is_empty() {
        return layout_math_text("", fonts, script);
    }

    let mut children: Vec<MathLayout> = nodes
        .iter()
        .map(|node| layout_math(node, fonts, script))
        .collect();
    let baseline = children
        .iter()
        .map(|child| child.baseline)
        .max()
        .unwrap_or(0);
    let below = children
        .iter()
        .map(|child| child.height.saturating_sub(child.baseline))
        .max()
        .unwrap_or(0);

    let mut x = 0usize;
    let mut items = Vec::with_capacity(children.len());
    for child in children.drain(..) {
        let y = baseline.saturating_sub(child.baseline);
        let width = child.width;
        items.push(PositionedMathBox { x, y, item: child });
        x += width;
    }

    MathLayout {
        width: x,
        height: baseline + below,
        baseline,
        kind: MathLayoutKind::Row(items),
    }
}

fn layout_math_supsub(
    base: &MathNode,
    sup: &Option<Box<MathNode>>,
    sub: &Option<Box<MathNode>>,
    fonts: &FontSet<'_>,
    script: bool,
) -> MathLayout {
    let base_layout = layout_math(base, fonts, script);
    let sup_layout = sup.as_ref().map(|node| layout_math(node, fonts, true));
    let sub_layout = sub.as_ref().map(|node| layout_math(node, fonts, true));

    let script_width = sup_layout
        .as_ref()
        .map(|layout| layout.width)
        .unwrap_or(0)
        .max(sub_layout.as_ref().map(|layout| layout.width).unwrap_or(0));
    let script_gap = 1usize;

    let base_top = -(base_layout.baseline as isize);
    let base_bottom = base_top + base_layout.height as isize;
    let sup_top = sup_layout
        .as_ref()
        .map(|layout| -(base_layout.baseline as isize) - (layout.height as isize / 2))
        .unwrap_or(base_top);
    let sub_top = sub_layout
        .as_ref()
        .map(|_| (base_layout.height.saturating_sub(base_layout.baseline) / 2) as isize)
        .unwrap_or(base_top);
    let min_top = base_top.min(sup_top).min(sub_top);
    let max_bottom = base_bottom
        .max(
            sup_layout
                .as_ref()
                .map(|layout| sup_top + layout.height as isize)
                .unwrap_or(base_bottom),
        )
        .max(
            sub_layout
                .as_ref()
                .map(|layout| sub_top + layout.height as isize)
                .unwrap_or(base_bottom),
        );
    let baseline = (-min_top) as usize;
    let base_y = (base_top - min_top) as usize;
    let sup_y = (sup_top - min_top) as usize;
    let sub_y = (sub_top - min_top) as usize;
    let script_x = base_layout.width + script_gap;

    let base_item = PositionedMathBox {
        x: 0,
        y: base_y,
        item: base_layout,
    };
    let sup_item = sup_layout.map(|item| PositionedMathBox {
        x: script_x,
        y: sup_y,
        item,
    });
    let sub_item = sub_layout.map(|item| PositionedMathBox {
        x: script_x,
        y: sub_y,
        item,
    });

    let mut items = vec![base_item];
    if let Some(item) = sup_item {
        items.push(item);
    }
    if let Some(item) = sub_item {
        items.push(item);
    }

    MathLayout {
        width: script_x + script_width,
        height: (max_bottom - min_top) as usize,
        baseline,
        kind: MathLayoutKind::Row(items),
    }
}

fn layout_math_fraction(
    numerator: &MathNode,
    denominator: &MathNode,
    fonts: &FontSet<'_>,
) -> MathLayout {
    let numerator = layout_math(numerator, fonts, true);
    let denominator = layout_math(denominator, fonts, true);
    let gap = 2usize;
    let bar_y = numerator.height + gap;
    let width = numerator.width.max(denominator.width).max(6) + 4;
    let numerator_x = (width - numerator.width) / 2;
    let denominator_x = (width - denominator.width) / 2;
    let denominator_y = bar_y + 1 + gap;
    let height = denominator_y + denominator.height;
    let baseline = denominator_y + denominator.baseline;

    MathLayout {
        width,
        height,
        baseline,
        kind: MathLayoutKind::Fraction {
            numerator: Box::new(PositionedMathBox {
                x: numerator_x,
                y: 0,
                item: numerator,
            }),
            denominator: Box::new(PositionedMathBox {
                x: denominator_x,
                y: denominator_y,
                item: denominator,
            }),
            bar_y,
        },
    }
}

fn layout_math_sqrt(body: &MathNode, fonts: &FontSet<'_>) -> MathLayout {
    let body = layout_math(body, fonts, false);
    let sign_width = fonts.math.text_width("\u{221a}");
    let body_x = sign_width + 2;
    let body_y = 2usize;
    let sign_height = fonts.math.glyph_height as usize;
    let sign_baseline = math_text_baseline(fonts.math);
    let width = body_x + body.width + 2;
    let height = sign_height.max(body_y + body.height);
    let baseline = sign_baseline.max(body_y + body.baseline);

    MathLayout {
        width,
        height,
        baseline,
        kind: MathLayoutKind::Sqrt {
            body: Box::new(PositionedMathBox {
                x: body_x,
                y: body_y,
                item: body,
            }),
            overbar_x: body_x,
            overbar_y: 0,
            overbar_width: width.saturating_sub(body_x),
        },
    }
}

fn math_font_for_role<'a>(fonts: &'a FontSet<'_>, role: MathFontRole) -> &'a Font {
    match role {
        MathFontRole::Math => fonts.math,
        MathFontRole::Script => fonts.script,
    }
}

pub(super) fn math_text_baseline(font: &Font) -> usize {
    font.baseline()
}

fn tex_symbol(command: &str) -> Option<&'static str> {
    match command {
        "alpha" => Some("\u{03b1}"),
        "beta" => Some("\u{03b2}"),
        "gamma" => Some("\u{03b3}"),
        "delta" => Some("\u{03b4}"),
        "epsilon" => Some("\u{03b5}"),
        "varepsilon" => Some("\u{03f5}"),
        "zeta" => Some("\u{03b6}"),
        "eta" => Some("\u{03b7}"),
        "theta" => Some("\u{03b8}"),
        "vartheta" => Some("\u{03d1}"),
        "iota" => Some("\u{03b9}"),
        "kappa" => Some("\u{03ba}"),
        "lambda" => Some("\u{03bb}"),
        "mu" => Some("\u{03bc}"),
        "nu" => Some("\u{03bd}"),
        "xi" => Some("\u{03be}"),
        "pi" => Some("\u{03c0}"),
        "rho" => Some("\u{03c1}"),
        "sigma" => Some("\u{03c3}"),
        "tau" => Some("\u{03c4}"),
        "upsilon" => Some("\u{03c5}"),
        "phi" => Some("\u{03c6}"),
        "varphi" => Some("\u{03d5}"),
        "chi" => Some("\u{03c7}"),
        "psi" => Some("\u{03c8}"),
        "omega" => Some("\u{03c9}"),
        "Gamma" => Some("\u{0393}"),
        "Delta" => Some("\u{0394}"),
        "Theta" => Some("\u{0398}"),
        "Lambda" => Some("\u{039b}"),
        "Xi" => Some("\u{039e}"),
        "Pi" => Some("\u{03a0}"),
        "Sigma" => Some("\u{03a3}"),
        "Upsilon" => Some("\u{03a5}"),
        "Phi" => Some("\u{03a6}"),
        "Psi" => Some("\u{03a8}"),
        "Omega" => Some("\u{03a9}"),
        "times" => Some("\u{00d7}"),
        "cdot" => Some("\u{00b7}"),
        "pm" => Some("\u{00b1}"),
        "mp" => Some("\u{2213}"),
        "le" | "leq" => Some("\u{2264}"),
        "ge" | "geq" => Some("\u{2265}"),
        "ne" | "neq" => Some("\u{2260}"),
        "approx" => Some("\u{2248}"),
        "infty" => Some("\u{221e}"),
        "partial" => Some("\u{2202}"),
        "nabla" => Some("\u{2207}"),
        "sum" => Some("\u{2211}"),
        "prod" => Some("\u{220f}"),
        "int" => Some("\u{222b}"),
        "sqrt" => Some("\u{221a}"),
        "to" | "rightarrow" => Some("\u{2192}"),
        "leftarrow" => Some("\u{2190}"),
        "Rightarrow" => Some("\u{21d2}"),
        "Leftarrow" => Some("\u{21d0}"),
        "in" => Some("\u{2208}"),
        "notin" => Some("\u{2209}"),
        "subset" => Some("\u{2282}"),
        "subseteq" => Some("\u{2286}"),
        "cup" => Some("\u{222a}"),
        "cap" => Some("\u{2229}"),
        _ => None,
    }
}
