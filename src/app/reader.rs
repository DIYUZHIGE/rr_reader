use super::{Activity, ReaderApp};
use crate::browser::{file_browser_parts, resolve_wiki_link_target, truncate_for_width};
use crate::display::Display;
use crate::font::{Font, FontSet};
use crate::hardware::Hardware;
use crate::power::PowerManager;
use crate::reader::{draw_reader_page, ReaderCache, ReaderState, READER_X};
use anyhow::Result;
use esp_idf_hal::sys;
use log::{debug, info, warn};

const CONTENT_TOP: usize = 12;
const LIST_X: usize = 24;
const LIST_RIGHT_MARGIN: usize = 16;

impl ReaderApp {
    pub(super) fn open_random_file(&mut self) {
        if self.md_files.is_empty() {
            return;
        }
        let idx = (crate::time::now_ms() as usize) % self.md_files.len();
        self.on_enter_reader_mode();
        self.reader_history.clear();
        self.activity = Activity::Reader(ReaderState {
            file_index: idx,
            page_index: 0,
            wiki_link_selected: None,
        });
        self.render_current_file();
    }

    pub(super) fn on_enter_reader_mode(&mut self) {
        self.reader_cache = None;
        self.display.clear_glyph_cache();
        if !self.wifi_suspended_for_reader {
            self.hardware.suspend_wifi_for_reader();
            self.wifi_suspended_for_reader = true;
        }
    }

    pub(super) fn on_exit_reader_mode(&mut self) {
        if self.wifi_suspended_for_reader {
            self.hardware.resume_wifi_after_reader();
            self.wifi_suspended_for_reader = false;
        }
    }

    pub(super) fn move_reader_page(&mut self, delta: isize) {
        let reader = match self.activity {
            Activity::Reader(reader) => reader,
            _ => return,
        };

        if self.ensure_reader_cache(reader.file_index).is_err() {
            return;
        }

        let page_count = self.reader_page_count().max(1);
        let next = if delta.is_negative() {
            reader
                .page_index
                .saturating_sub(delta.unsigned_abs().min(reader.page_index))
        } else {
            reader
                .page_index
                .saturating_add(delta as usize)
                .min(page_count - 1)
        };

        if next == reader.page_index {
            return;
        }

        self.activity = Activity::Reader(ReaderState {
            file_index: reader.file_index,
            page_index: next,
            wiki_link_selected: None,
        });
        self.render_current_file();
    }

    pub(super) fn render_current_file(&mut self) {
        self.display.clear(0xFF);

        if self.md_files.is_empty() {
            self.display.draw_text_wrapped(
                &self.ui_font,
                "没有 Markdown 文件\n/sdcard/vault",
                READER_X,
                20,
                Display::width() - READER_X,
                4,
            );
            debug!("No files to render");
            return;
        }

        let reader = match &self.activity {
            Activity::Reader(reader) => Some(*reader),
            _ => None,
        };

        let file_index = match self.activity {
            Activity::Reader(reader) => reader.file_index.min(self.md_files.len() - 1),
            _ => 0,
        };
        let rel_path = self.md_files[file_index].clone();
        match self.ensure_reader_cache(file_index) {
            Ok(()) => {
                let page_count = self.reader_page_count().max(1);
                let page_index = reader
                    .map(|reader| reader.page_index.min(page_count - 1))
                    .unwrap_or(0);
                if let Some(reader) = reader {
                    if page_index != reader.page_index {
                        self.activity = Activity::Reader(ReaderState {
                            file_index,
                            page_index,
                            wiki_link_selected: None,
                        });
                    }
                }

                let fonts = FontSet::new(
                    &self.ui_font,
                    &self.reader_font,
                    &self.math_font,
                    &self.script_font,
                );
                if let Some(cache) = self.reader_cache.as_mut() {
                    cache.load_page(page_index);
                }

                let (_, name) = file_browser_parts(&rel_path);
                let title = format!("[{}] {}", file_index + 1, name);

                let page = self
                    .reader_cache
                    .as_mut()
                    .and_then(|c| c.load_page(page_index));
                if let Some(page) = page {
                    let selected = reader.and_then(|r| r.wiki_link_selected);
                    draw_reader_page(&mut self.display, &fonts, page, selected, |image_path| {
                        self.hardware
                            .storage
                            .resolve_asset_path_relative_to(&rel_path, image_path)
                    });
                }
                let footer = format!("{}/{}", page_index + 1, page_count);
                let title = truncate_for_width(
                    &self.ui_font,
                    &title,
                    Display::width()
                        - LIST_X
                        - LIST_RIGHT_MARGIN
                        - self.ui_font.text_width(&footer)
                        - 12,
                );
                self.draw_bottom_bar(&title, &footer);
                debug!(
                    "Rendering file {}/{} page {}/{}: {}",
                    file_index + 1,
                    self.md_files.len(),
                    page_index + 1,
                    page_count,
                    rel_path
                );
            }
            Err(e) => {
                self.display.draw_text_wrapped(
                    &self.ui_font,
                    &format!("Error reading {}:\n{}", rel_path, e),
                    READER_X,
                    20,
                    Display::width() - READER_X,
                    4,
                );
                warn!("Failed to read {}: {}", rel_path, e);
            }
        }
    }

    pub(super) fn ensure_reader_cache(&mut self, file_index: usize) -> Result<()> {
        if matches!(
            self.reader_cache,
            Some(ReaderCache {
                file_index: cached,
                ..
            }) if cached == file_index
        ) {
            return Ok(());
        }

        // Free heap before heavy markdown parsing
        self.reader_cache = None;
        self.display.clear_glyph_cache();

        #[cfg(debug_assertions)]
        {
            let free_before = unsafe { esp_idf_hal::sys::esp_get_free_heap_size() };
            info!("Free heap before parsing: {} bytes", free_before);
        }

        let rel_path = &self.md_files[file_index];
        let full_path = format!("/sdcard/vault/{}", rel_path);
        let fonts = FontSet::new(
            &self.ui_font,
            &self.reader_font,
            &self.math_font,
            &self.script_font,
        );

        let cache = crate::reader::ReaderCache::load(file_index, &full_path, &fonts)?;
        self.reader_cache = Some(cache);

        #[cfg(debug_assertions)]
        {
            let free_after = unsafe { esp_idf_hal::sys::esp_get_free_heap_size() };
            let min_free = unsafe { esp_idf_hal::sys::esp_get_minimum_free_heap_size() };
            info!(
                "Free heap after parsing: {} bytes (min ever: {})",
                free_after, min_free
            );
        }

        Ok(())
    }

    pub(super) fn reader_page_count(&self) -> usize {
        self.reader_cache
            .as_ref()
            .map(|cache| cache.page_count)
            .unwrap_or(0)
    }

    pub(super) fn cycle_wiki_link_selection(&mut self) {
        let (file_index, page_index) = match &self.activity {
            Activity::Reader(r) => (r.file_index, r.page_index),
            _ => return,
        };

        let link_count = self
            .reader_cache
            .as_mut()
            .and_then(|c| c.load_page(page_index))
            .map(|p| p.wiki_links.len())
            .unwrap_or(0);

        if link_count == 0 {
            // No wiki links: re-render (original behavior)
            self.render_current_file();
            return;
        }

        let current = match &self.activity {
            Activity::Reader(r) => r.wiki_link_selected,
            _ => None,
        };

        let next = match current {
            None => Some(0),
            Some(i) if i + 1 < link_count => Some(i + 1),
            Some(_) => None, // wrap: last → none (deselect)
        };

        self.activity = Activity::Reader(ReaderState {
            file_index,
            page_index,
            wiki_link_selected: next,
        });
        self.render_current_file();
    }

    /// Navigate back to the previous file in the navigation history.
    pub(super) fn navigate_back_in_history(&mut self) {
        if let Some(prev_state) = self.reader_history.pop() {
            info!(
                "Navigating back: file {} page {}",
                prev_state.file_index, prev_state.page_index
            );
            self.on_enter_reader_mode();
            self.activity = Activity::Reader(prev_state);
            self.render_current_file();
        } else {
            // No history: return to file browser
            if let Activity::Reader(reader) = self.activity {
                self.on_exit_reader_mode();
                self.activity = Activity::FileBrowser;
                let selected_path = self.md_files.get(reader.file_index).cloned();
                self.reload_browser_entries(selected_path.as_deref());
                self.render_file_browser();
            }
        }
    }

    /// Attempt to follow the currently selected wiki link on the reader page.
    /// Returns true if a link was followed, false otherwise.
    pub(super) fn try_follow_wiki_link(&mut self) -> bool {
        let (_file_index, page_index, selected) = match &self.activity {
            Activity::Reader(r) => (r.file_index, r.page_index, r.wiki_link_selected),
            _ => return false,
        };

        let selected = match selected {
            Some(i) => i,
            None => {
                // No link selected; try the first one as a fallback
                0
            }
        };

        let page = match self
            .reader_cache
            .as_mut()
            .and_then(|c| c.load_page(page_index))
        {
            Some(p) => p,
            None => return false,
        };

        let link = match page.wiki_links.get(selected) {
            Some(l) => l,
            None => return false,
        };

        let target = link.target.clone();
        info!("Following wiki link: [[{}]]", target);

        match resolve_wiki_link_target(&target, &self.md_files) {
            Some(idx) => {
                // Push current state to history before navigating
                if let Activity::Reader(current) = self.activity {
                    self.reader_history.push(current);
                }
                self.on_enter_reader_mode();
                self.activity = Activity::Reader(ReaderState {
                    file_index: idx,
                    page_index: 0,
                    wiki_link_selected: None,
                });
                self.render_current_file();
                true
            }
            None => {
                info!("Wiki link target not found: [[{}]]", target);
                false
            }
        }
    }
}
