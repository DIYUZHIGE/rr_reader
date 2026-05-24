use crate::browser::truncate_for_width;
use crate::display::Display;
use crate::font::Font;
use crate::network::WifiStatus;
use crate::sync;
use anyhow::Result;
use log::{debug, info, warn};

const CONTENT_TOP: usize = 12;
const LIST_X: usize = 24;
const LIST_RIGHT_MARGIN: usize = 16;

impl super::ReaderApp {
    pub(super) fn open_settings_page(&mut self) {
        if !matches!(self.activity, super::Activity::FileBrowser) {
            return;
        }
        self.settings_selected = 0;
        self.activity = super::Activity::Settings;
        self.render_settings_page();
    }

    pub(super) fn settings_option_count(&self) -> usize {
        6
    }

    pub(super) fn move_settings_selection(&mut self, delta: isize) {
        if !matches!(self.activity, super::Activity::Settings) {
            return;
        }

        let count = self.settings_option_count();
        self.settings_selected =
            (self.settings_selected as isize + delta).rem_euclid(count as isize) as usize;
        self.render_settings_page();
    }

    pub(super) fn confirm_settings_selection(&mut self) {
        if !matches!(self.activity, super::Activity::Settings) {
            return;
        }

        match self.settings_selected {
            0 => self.trigger_manual_sync(),
            1 => self.trigger_delete_notes_and_sync(),
            2 => self.trigger_clear_page_cache(),
            3 => self.trigger_clear_image_cache(),
            4 => self.trigger_cache_all_images(),
            5 => self.enter_wifi_settings(),
            _ => {}
        }
    }

    pub(super) fn render_sync_status_light(&mut self, status: &str) {
        self.display.clear(0xFF);
        let title = "同步中...";
        let title_w = self.ui_font.text_width(title);
        let title_x = (Display::width() - title_w) / 2;
        let title_y = (Display::height() - self.ui_font.glyph_height as usize) / 2 - 10;
        self.display
            .draw_text_font(&self.ui_font, title, title_x, title_y);
        let status_text = format!("状态: {}", status);
        let status_text = truncate_for_width(
            &self.ui_font,
            &status_text,
            Display::width() - LIST_X - LIST_RIGHT_MARGIN,
        );
        self.draw_bottom_bar(&status_text, "");
    }

    pub(super) fn trigger_manual_sync(&mut self) {
        // Aggressively release reader-related caches before network sync.
        self.reader_cache = None;
        self.display.clear_glyph_cache();

        let wifi_status = self.hardware.connect_wifi_from_storage();
        info!("{}", wifi_status.boot_line());

        let cfg = self.hardware.storage.read_remotely_save_config();
        let final_status = match cfg {
            Ok(Some(cfg)) => {
                info!(
                    "Sync config loaded: endpoint={} region={} bucket={} prefix={} force_path_style={} source={}",
                    cfg.endpoint,
                    cfg.region,
                    cfg.bucket_name,
                    cfg.remote_prefix,
                    cfg.force_path_style,
                    cfg.source_path
                );

                // Initial status
                self.settings_status = "正在连接...".to_string();
                self.render_sync_status_light("正在连接...");
                self.flush_ui_refresh();
                self.display.clear_glyph_cache();

                match sync::sync_vault_from_s3_config(&cfg, &mut |msg: &str| {
                    self.settings_status = msg.to_string();
                    self.render_sync_status_light(msg);
                    self.flush_ui_refresh();
                    self.display.clear_glyph_cache();
                }) {
                    Ok(report) => {
                        self.md_files = match self.hardware.storage.list_markdown_files("") {
                            Ok(files) => {
                                info!("Rescan after sync: {} files", files.len());
                                files
                            }
                            Err(e) => {
                                warn!("Failed to rescan vault after sync: {}", e);
                                self.md_files.clone()
                            }
                        };
                        self.reload_browser_entries(None);
                        info!("Sync status written to {}", report.status_path);
                        format!(
                            "同步完成：下载 {}，跳过 {}，删除 {}",
                            report.downloaded_files, report.skipped_files, report.deleted_files
                        )
                    }
                    Err(e) => {
                        warn!("Sync failed: {}", e);
                        format!("同步失败: {}", e)
                    }
                }
            }
            Ok(None) => "未找到 remotely-save 配置文件".to_string(),
            Err(e) => {
                warn!("Failed to load remotely-save config: {}", e);
                format!("读取同步配置失败: {}", e)
            }
        };

        self.hardware.shutdown_wifi_after_sync();
        // Aggressively free all caches after sync.
        self.reader_cache = None;
        self.display.clear_glyph_cache();
        self.settings_status = final_status;
        self.render_settings_page();
    }

    pub(super) fn trigger_delete_notes_and_sync(&mut self) {
        self.reader_cache = None;
        self.display.clear_glyph_cache();

        self.settings_status = "正在删除本地 notes...".to_string();
        self.render_sync_status_light("正在删除本地文件...");
        self.flush_ui_refresh();
        self.display.clear_glyph_cache();

        match self.hardware.storage.delete_synced_notes() {
            Ok(()) => {
                self.md_files.clear();
                self.reload_browser_entries(None);
                self.trigger_manual_sync();
            }
            Err(e) => {
                warn!("Failed to delete local synced notes: {}", e);
                self.settings_status = format!("删除 notes 失败: {}", e);
                self.render_settings_page();
            }
        }
    }

    pub(super) fn trigger_clear_page_cache(&mut self) {
        self.settings_status = match self.hardware.storage.clear_page_cache() {
            Ok(()) => "页面缓存已清除".to_string(),
            Err(e) => format!("清除缓存失败: {}", e),
        };
        self.render_settings_page();
    }

    pub(super) fn trigger_clear_image_cache(&mut self) {
        let removed = crate::reader::clear_image_cache();
        self.settings_status = format!("已清除 {} 个图片缓存", removed);
        self.render_settings_page();
    }

    pub(super) fn trigger_cache_all_images(&mut self) {
        self.reader_cache = None;
        self.display.clear_glyph_cache();
        self.settings_status = "正在缓存图片...".to_string();
        self.render_settings_page();
        self.flush_ui_refresh();

        let cached = crate::reader::cache_all_images(&mut |msg: &str| {
            self.settings_status = msg.to_string();
            self.render_settings_page();
            self.flush_ui_refresh();
            self.display.clear_glyph_cache();
        });
        self.settings_status = format!("图片缓存完成: {} 张", cached);
        self.render_settings_page();
    }

    pub(super) fn render_settings_page(&mut self) {
        self.display.clear(0xFF);

        let current_root = if self.browser_root_dir.is_empty() {
            "/ (vault 根)".to_string()
        } else {
            format!("/{}", self.browser_root_dir)
        };

        let options = [
            "手动同步（S3）",
            "删除本地文件并重新同步",
            "清除页面缓存",
            "清除图片缓存",
            "缓存所有图片",
            "Wi-Fi 设置",
        ];

        for (i, option) in options.iter().enumerate() {
            let y = CONTENT_TOP + i * (self.ui_font.glyph_height as usize + 10);
            if i == self.settings_selected {
                self.display
                    .fill_rect(12, y, 2, self.ui_font.glyph_height as usize + 4, 0x00);
            }
            self.display
                .draw_text_font(&self.ui_font, option, LIST_X, y);
        }

        let root_line = format!("当前: {}", current_root);
        let right = if self.settings_status.is_empty() {
            String::new()
        } else {
            let status = truncate_for_width(
                &self.ui_font,
                &self.settings_status,
                Display::width()
                    - LIST_X
                    - LIST_RIGHT_MARGIN
                    - self.ui_font.text_width(&root_line)
                    - 12,
            );
            status.to_string()
        };
        self.draw_bottom_bar(&root_line, &right);
    }
}
