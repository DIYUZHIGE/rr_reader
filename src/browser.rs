use crate::font::Font;
use std::borrow::Cow;

#[derive(Clone, Debug)]
pub struct BrowserEntry {
    pub rel_path: String,
    pub is_dir: bool,
}

impl BrowserEntry {
    /// Extract the display name from the relative path.
    /// For "foo/bar.md", returns "bar". For "foo/baz", returns "baz".
    pub fn display_name(&self) -> &str {
        let file_name = self.rel_path.rsplit('/').next().unwrap_or(&self.rel_path);
        strip_markdown_extension(file_name)
    }
}

#[derive(Clone, Debug)]
pub struct FileBrowserState {
    pub current_dir: String,
    pub entries: Vec<BrowserEntry>,
    pub selected: usize,
    pub first_visible: usize,
}

impl FileBrowserState {
    pub fn new_root() -> Self {
        Self {
            current_dir: String::new(),
            entries: Vec::new(),
            selected: 0,
            first_visible: 0,
        }
    }

    pub fn parent_dir(&self) -> String {
        if self.current_dir.is_empty() {
            return String::new();
        }

        match self.current_dir.rsplit_once('/') {
            Some((parent, _)) => parent.to_string(),
            None => String::new(),
        }
    }
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

pub fn sort_browser_entries(entries: &mut [BrowserEntry]) {
    entries.sort_by_cached_key(|entry| {
        (
            !entry.is_dir,
            entry.display_name().to_lowercase(),
            entry.rel_path.to_lowercase(),
        )
    });
}

pub fn file_browser_parts(path: &str) -> (&str, &str) {
    let (folder, file_name) = match path.rsplit_once('/') {
        Some((folder, file_name)) if !folder.is_empty() => (folder, file_name),
        _ => ("根目录", path),
    };

    (folder, strip_markdown_extension(file_name))
}

/// Resolve an Obsidian wiki link target to a file index in the md_files list.
/// The target is typically a page name like "Projects/rr_reader" without extension.
/// Returns the index in md_files if a matching file is found.
pub fn resolve_wiki_link_target(target: &str, md_files: &[String]) -> Option<usize> {
    let target_normalized = target.replace('\\', "/");

    // 1. Try exact match with .md extension
    let with_md = format!("{}.md", target_normalized);
    if let Some(idx) = md_files.iter().position(|p| p == &with_md) {
        return Some(idx);
    }

    // 2. Try exact match with .markdown extension
    let with_markdown = format!("{}.markdown", target_normalized);
    if let Some(idx) = md_files.iter().position(|p| p == &with_markdown) {
        return Some(idx);
    }

    // 3. Try case-insensitive match with extensions
    let lower_target = target_normalized.to_lowercase();
    for (idx, path) in md_files.iter().enumerate() {
        let lower_path = path.to_lowercase();
        if lower_path == format!("{}.md", lower_target)
            || lower_path == format!("{}.markdown", lower_target)
        {
            return Some(idx);
        }
    }

    // 4. Try matching the target as a suffix of some file path
    //    e.g., target="rr_reader" matches "Projects/rr_reader.md"
    let target_last = target_normalized
        .rsplit('/')
        .next()
        .unwrap_or(&target_normalized);
    let lower_last = target_last.to_lowercase();
    for (idx, path) in md_files.iter().enumerate() {
        let (_, name) = file_browser_parts(path);
        if name.to_lowercase() == lower_last {
            return Some(idx);
        }
    }

    None
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
