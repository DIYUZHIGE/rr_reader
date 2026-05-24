use crate::browser::{file_browser_parts, sort_browser_entries, truncate_for_width, BrowserEntry};
use crate::display::Display;
use crate::font::Font;
use crate::hardware::Hardware;
use crate::power::PowerManager;
use crate::reader::ReaderState;
use anyhow::Result;
use log::{debug, info};
use std::collections::BTreeSet;

use super::Activity;

const CONTENT_TOP: usize = 12;
const LIST_X: usize = 24;
const LIST_RIGHT_MARGIN: usize = 16;

impl super::ReaderApp {
    pub(super) fn browser_is_at_root(&self) -> bool {
        self.browser.current_dir == self.browser_root_dir
    }

    pub(super) fn absolute_prefix_for_current_dir(&self) -> String {
        if self.browser.current_dir.is_empty() {
            String::new()
        } else {
            format!("{}/", self.browser.current_dir)
        }
    }

    pub(super) fn reload_browser_entries(&mut self, selected_rel_path: Option<&str>) {
        let current_dir = self.browser.current_dir.clone();
        let root_prefix = if self.browser_root_dir.is_empty() {
            String::new()
        } else {
            format!("{}/", self.browser_root_dir)
        };
        let current_prefix = self.absolute_prefix_for_current_dir();
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        let mut files: Vec<BrowserEntry> = Vec::new();

        for rel in &self.md_files {
            if !root_prefix.is_empty()
                && !(rel == &self.browser_root_dir || rel.starts_with(&root_prefix))
            {
                continue;
            }

            if !current_prefix.is_empty() && !rel.starts_with(&current_prefix) {
                continue;
            }

            let rel_from_dir = if current_dir.is_empty() {
                rel.as_str()
            } else {
                &rel[current_dir.len() + 1..]
            };

            if let Some((child_dir, _)) = rel_from_dir.split_once('/') {
                let child_rel_path = if current_dir.is_empty() {
                    child_dir.to_string()
                } else {
                    format!("{}/{}", current_dir, child_dir)
                };
                dirs.insert(child_rel_path);
            } else {
                files.push(BrowserEntry {
                    rel_path: rel.clone(),
                    is_dir: false,
                });
            }
        }

        let mut entries: Vec<BrowserEntry> = dirs
            .into_iter()
            .map(|rel_path| BrowserEntry {
                rel_path,
                is_dir: true,
            })
            .collect();
        entries.extend(files);
        sort_browser_entries(&mut entries);

        debug!(
            "reload_browser_entries: md_files={} root_prefix={:?} current_dir={:?} entries={} (dirs={} files={})",
            self.md_files.len(),
            root_prefix,
            self.browser.current_dir,
            entries.len(),
            entries.iter().filter(|e| e.is_dir).count(),
            entries.iter().filter(|e| !e.is_dir).count()
        );

        self.browser.entries = entries;
        self.browser.selected = 0;
        self.browser.first_visible = 0;

        if let Some(selected_rel_path) = selected_rel_path {
            if let Some(idx) = self
                .browser
                .entries
                .iter()
                .position(|entry| entry.rel_path == selected_rel_path)
            {
                self.set_browser_selection(idx);
            }
        }
    }

    pub(super) fn move_browser_selection(&mut self, delta: isize) {
        if !matches!(self.activity, Activity::FileBrowser) || self.browser.entries.is_empty() {
            return;
        }

        let count = self.browser.entries.len();
        let next = (self.browser.selected as isize + delta).rem_euclid(count as isize) as usize;
        self.set_browser_selection(next);
        self.render_file_browser();
    }

    pub(super) fn set_browser_selection(&mut self, selected: usize) {
        let row_count = self.browser_row_count().max(1);
        self.browser.selected = selected.min(self.browser.entries.len().saturating_sub(1));

        if self.browser.selected < self.browser.first_visible {
            self.browser.first_visible = self.browser.selected;
        } else if self.browser.selected >= self.browser.first_visible + row_count {
            self.browser.first_visible = self.browser.selected + 1 - row_count;
        }
    }

    pub(super) fn open_selected_file(&mut self) {
        if !matches!(self.activity, Activity::FileBrowser) {
            return;
        }

        let entry = match self.browser.entries.get(self.browser.selected).cloned() {
            Some(entry) => entry,
            None => return,
        };

        if entry.is_dir {
            self.browser.current_dir = entry.rel_path;
            self.reload_browser_entries(None);
            self.render_file_browser();
            return;
        }

        let file_index = match self
            .md_files
            .iter()
            .position(|path| path == &entry.rel_path)
        {
            Some(index) => index,
            None => return,
        };

        self.on_enter_reader_mode();
        self.reader_history.clear();
        self.activity = Activity::Reader(ReaderState {
            file_index,
            page_index: 0,
            wiki_link_selected: None,
        });
        self.render_current_file();
    }

    pub(super) fn handle_browser_back(&mut self) {
        if !matches!(self.activity, Activity::FileBrowser) {
            return;
        }

        if self.browser_is_at_root() {
            return;
        }

        let child_dir = self.browser.current_dir.clone();
        let parent = self.browser.parent_dir();
        self.browser.current_dir = if parent.len() < self.browser_root_dir.len() {
            self.browser_root_dir.clone()
        } else {
            parent
        };
        self.reload_browser_entries(Some(&child_dir));
        self.render_file_browser();
    }

    pub(super) fn render_file_browser(&mut self) {
        self.display.clear_glyph_cache();
        self.display.clear(0xFF);

        let path_title = if self.browser.current_dir.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", self.browser.current_dir)
        };

        if self.browser.entries.is_empty() {
            self.display.draw_text_wrapped(
                &self.ui_font,
                "此目录为空",
                LIST_X,
                CONTENT_TOP,
                Display::width() - LIST_RIGHT_MARGIN,
                3,
            );
            self.draw_bottom_bar(&path_title, "");
            return;
        }

        let selected = self.browser.selected;
        let first_visible = self.browser.first_visible;

        let row_height = self.browser_row_height();
        let row_count = self.browser_row_count();
        let end = (first_visible + row_count).min(self.browser.entries.len());

        for (row, idx) in (first_visible..end).enumerate() {
            let y = CONTENT_TOP + row * row_height;
            let selected_row = idx == selected;
            if selected_row {
                self.display.fill_rect(12, y, 2, row_height - 6, 0x00);
            }

            let entry = &self.browser.entries[idx];
            let display_name = if entry.is_dir {
                format!("{} /", entry.display_name())
            } else {
                entry.display_name().to_string()
            };

            let name = truncate_for_width(
                &self.ui_font,
                &display_name,
                Display::width() - LIST_X - LIST_RIGHT_MARGIN,
            );
            self.display.draw_text_font(&self.ui_font, &name, LIST_X, y);
        }

        self.draw_bottom_bar(&path_title, "");

        debug!(
            "Rendering file browser: dir='{}' selected {}/{}",
            self.browser.current_dir,
            selected + 1,
            self.browser.entries.len()
        );
    }

    pub(super) fn browser_row_count(&self) -> usize {
        let row_height = self.browser_row_height();
        let bottom_reserved = self.bottom_bar_total_height();
        (Display::height() - CONTENT_TOP - bottom_reserved) / row_height
    }

    pub(super) fn browser_row_height(&self) -> usize {
        self.ui_font.glyph_height as usize + 6
    }
}
