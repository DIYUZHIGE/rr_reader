use crate::font::Font;
use std::borrow::Cow;

#[derive(Clone, Copy, Debug)]
pub struct FileBrowserState {
    pub selected: usize,
    pub first_visible: usize,
}

pub fn truncate_for_width<'a>(font: &Font, text: &'a str, max_px: usize) -> Cow<'a, str> {
    if font.text_width(text) <= max_px {
        return Cow::Borrowed(text);
    }

    const ELLIPSIS: &str = "...";
    let ellipsis_width = font.text_width(ELLIPSIS);
    if max_px <= ellipsis_width {
        let dot_count = (max_px / font.char_advance_width('.')).max(1);
        let buf = [b'.'; 12];
        let len = dot_count.min(buf.len());
        return Cow::Owned(String::from_utf8_lossy(&buf[..len]).into_owned());
    }

    let mut out = String::with_capacity(text.len().min(64));
    let mut width = 0;
    let limit = max_px - ellipsis_width;
    for ch in text.chars() {
        let advance = font.char_advance_width(ch);
        if width + advance > limit {
            break;
        }
        out.push(ch);
        width += advance;
    }
    out.push_str(ELLIPSIS);
    Cow::Owned(out)
}

pub fn sort_markdown_files(files: &mut [String]) {
    files.sort_by_cached_key(|path| {
        let (folder, name) = file_browser_parts(path);
        (folder.to_lowercase(), name.to_lowercase(), path.to_string())
    });
}

pub fn file_browser_parts(path: &str) -> (&str, &str) {
    let (folder, file_name) = match path.rsplit_once('/') {
        Some((folder, file_name)) if !folder.is_empty() => (folder, file_name),
        _ => ("根目录", path),
    };

    (folder, strip_markdown_extension(file_name))
}

fn strip_markdown_extension(file_name: &str) -> &str {
    let lower = file_name.to_lowercase();
    if lower.ends_with(".markdown") {
        &file_name[..file_name.len() - ".markdown".len()]
    } else if lower.ends_with(".md") {
        &file_name[..file_name.len() - ".md".len()]
    } else {
        file_name
    }
}
