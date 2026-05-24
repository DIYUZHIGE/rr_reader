use crate::browser::truncate_for_width;
use crate::display::Display;
use crate::font::Font;
use crate::network::{AccessPointInfo, WifiStatus};
use anyhow::Result;
use esp_idf_svc::wifi::AuthMethod;
use log::{debug, info, warn};

use super::ReaderApp;

const WIFI_SIGNAL_X_OFFSET: usize = 340;
const WIFI_PASSWORD_CHARS: &str =
    "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!@#$%^&*()-_=+[]{}\\|;:'\",.<>/?`~";

struct WifiState {
    phase: WifiPhase,
    scanned_aps: Vec<AccessPointInfo>,
    selected_index: usize,
    selected_ap_index: usize,
    password_buf: String,
    cursor_pos: usize,
    status_message: String,
}

impl WifiState {
    pub(super) fn new() -> Self {
        Self {
            phase: WifiPhase::Scanning,
            scanned_aps: Vec::new(),
            selected_index: 0,
            selected_ap_index: 0,
            password_buf: String::with_capacity(64),
            cursor_pos: 0,
            status_message: String::new(),
        }
    }
}

const WIFI_PRESET_PASSWORDS: &[&str] = &["liuzhaohui123", "88888888"];

#[derive(Clone, Debug)]
enum WifiPhase {
    Scanning,
    NetworkList,
    PasswordChoice,
    PasswordInput,
    Connecting,
    ConnectResult,
}

enum WifiConfirmAction {
    Render,
    Connect(String, String),
}

impl ReaderApp {
    pub(super) fn enter_wifi_settings(&mut self) {
        self.reader_cache = None;
        self.display.clear_glyph_cache();
        self.wifi_state = Some(WifiState::new());
        self.activity = super::Activity::WifiSettings;

        // Show scanning screen immediately
        self.render_wifi_settings_page();
        self.flush_ui_refresh();

        // Blocking scan
        self.perform_wifi_scan();
    }

    pub(super) fn leave_wifi_settings(&mut self) {
        self.hardware.disconnect_wifi();
        self.wifi_state = None;
        self.activity = super::Activity::Settings;
        self.settings_status = String::new();
        self.render_settings_page();
    }

    pub(super) fn perform_wifi_scan(&mut self) {
        match self.hardware.scan_wifi() {
            Ok(aps) => {
                let count = aps.len();
                if let Some(ref mut state) = self.wifi_state {
                    state.scanned_aps = aps;
                    state.selected_index = 0;
                    state.phase = WifiPhase::NetworkList;
                    state.status_message = format!("已扫描 {} 个网络", count);
                }
            }
            Err(e) => {
                if let Some(ref mut state) = self.wifi_state {
                    state.phase = WifiPhase::ConnectResult;
                    state.status_message = format!("扫描失败: {}", e);
                }
            }
        }
        self.render_wifi_settings_page();
    }

    pub(super) fn handle_wifi_move(&mut self, delta: isize) {
        let is_empty;
        let has_connect_result;
        {
            let state = match self.wifi_state.as_mut() {
                Some(s) => s,
                None => return,
            };
            match state.phase {
                WifiPhase::NetworkList => {
                    if state.scanned_aps.is_empty() {
                        return;
                    }
                    let count = state.scanned_aps.len();
                    state.selected_index =
                        (state.selected_index as isize + delta).rem_euclid(count as isize) as usize;
                    is_empty = false;
                    has_connect_result = false;
                }
                WifiPhase::PasswordChoice => {
                    let count = WIFI_PRESET_PASSWORDS.len() + 1; // +1 for manual input
                    state.selected_index =
                        (state.selected_index as isize + delta).rem_euclid(count as isize) as usize;
                    is_empty = false;
                    has_connect_result = false;
                }
                WifiPhase::ConnectResult => {
                    state.phase = WifiPhase::NetworkList;
                    is_empty = false;
                    has_connect_result = true;
                }
                _ => return,
            }
        }
        if !is_empty || has_connect_result {
            self.render_wifi_settings_page();
        }
    }

    pub(super) fn handle_wifi_confirm(&mut self) {
        let action = {
            let state = match self.wifi_state.as_mut() {
                Some(s) => s,
                None => return,
            };

            match state.phase {
                WifiPhase::NetworkList => {
                    if state.scanned_aps.is_empty() {
                        return;
                    }
                    let ap = &state.scanned_aps[state.selected_index];
                    let need_pw =
                        ap.auth_method.is_some() && ap.auth_method != Some(AuthMethod::None);

                    if need_pw {
                        state.selected_ap_index = state.selected_index;
                        state.phase = WifiPhase::PasswordChoice;
                        state.selected_index = 0;
                        WifiConfirmAction::Render
                    } else {
                        WifiConfirmAction::Connect(ap.ssid.to_string(), String::new())
                    }
                }
                WifiPhase::PasswordChoice => {
                    let ap_idx = state.selected_ap_index;
                    let ssid = state.scanned_aps[ap_idx].ssid.to_string();
                    if state.selected_index < WIFI_PRESET_PASSWORDS.len() {
                        WifiConfirmAction::Connect(
                            ssid,
                            WIFI_PRESET_PASSWORDS[state.selected_index].to_string(),
                        )
                    } else {
                        // "手动输入"
                        state.password_buf.clear();
                        state.cursor_pos = 0;
                        state.phase = WifiPhase::PasswordInput;
                        WifiConfirmAction::Render
                    }
                }
                WifiPhase::PasswordInput => {
                    let ssid = state
                        .scanned_aps
                        .get(state.selected_ap_index)
                        .map(|ap| ap.ssid.as_str().to_string())
                        .unwrap_or_default();
                    let password = state.password_buf.clone();
                    WifiConfirmAction::Connect(ssid, password)
                }
                WifiPhase::ConnectResult => {
                    state.phase = WifiPhase::NetworkList;
                    WifiConfirmAction::Render
                }
                _ => return,
            }
        };

        match action {
            WifiConfirmAction::Render => self.render_wifi_settings_page(),
            WifiConfirmAction::Connect(ssid, password) => self.start_wifi_connect(&ssid, &password),
        }
    }

    pub(super) fn handle_wifi_back(&mut self) {
        let action = {
            let state = match self.wifi_state.as_mut() {
                Some(s) => s,
                None => return,
            };

            match state.phase {
                WifiPhase::NetworkList => 1, // Leave WiFi settings
                WifiPhase::PasswordChoice => {
                    state.phase = WifiPhase::NetworkList;
                    2 // Back to network list
                }
                WifiPhase::PasswordInput => {
                    state.password_buf.clear();
                    state.cursor_pos = 0;
                    state.phase = WifiPhase::NetworkList;
                    2 // Re-render
                }
                WifiPhase::ConnectResult => {
                    state.phase = WifiPhase::NetworkList;
                    2 // Re-render
                }
                _ => 0, // No action
            }
        };

        match action {
            1 => self.leave_wifi_settings(),
            2 => self.render_wifi_settings_page(),
            _ => {}
        }
    }

    pub(super) fn handle_wifi_cursor_move(&mut self, delta: i8) {
        let needs_render = {
            let state = match self.wifi_state.as_mut() {
                Some(s) => s,
                None => return,
            };
            if !matches!(state.phase, WifiPhase::PasswordInput) {
                return;
            }
            if delta < 0 {
                state.cursor_pos = state.cursor_pos.saturating_sub(1);
            } else {
                state.cursor_pos = (state.cursor_pos + 1).min(state.password_buf.len());
            }
            true
        };
        if needs_render {
            self.render_wifi_settings_page();
        }
    }

    pub(super) fn handle_wifi_char_change(&mut self, delta: i8) {
        let needs_render = {
            let state = match self.wifi_state.as_mut() {
                Some(s) => s,
                None => return,
            };
            if !matches!(state.phase, WifiPhase::PasswordInput) {
                return;
            }

            let chars: Vec<char> = WIFI_PASSWORD_CHARS.chars().collect();
            if chars.is_empty() {
                return;
            }

            if state.cursor_pos > state.password_buf.len() {
                state.cursor_pos = state.password_buf.len();
            }

            if state.cursor_pos == state.password_buf.len() {
                state.password_buf.push(chars[0]);
                state.cursor_pos = state.password_buf.len();
            } else {
                let current_char = state.password_buf.as_bytes()[state.cursor_pos] as char;
                let current_idx = chars.iter().position(|&c| c == current_char).unwrap_or(0);
                let new_idx = (current_idx as isize + delta as isize)
                    .rem_euclid(chars.len() as isize) as usize;
                let new_char = chars[new_idx];
                let end = state.cursor_pos + 1;
                state
                    .password_buf
                    .replace_range(state.cursor_pos..end, &new_char.to_string());
            }
            true
        };
        if needs_render {
            self.render_wifi_settings_page();
        }
    }

    pub(super) fn start_wifi_connect(&mut self, ssid: &str, password: &str) {
        // Render connecting screen
        if let Some(ref mut state) = self.wifi_state {
            state.phase = WifiPhase::Connecting;
            state.status_message = format!("正在连接 {}...", ssid);
        }
        self.render_wifi_settings_page();
        self.flush_ui_refresh();
        self.display.clear_glyph_cache();

        let status = self.hardware.connect_wifi_with_credentials(ssid, password);

        if let Some(ref mut state) = self.wifi_state {
            match &status {
                WifiStatus::Connected {
                    ssid: connected_ssid,
                    ip,
                } => {
                    // Persist credentials so S3 sync can use them
                    if !password.is_empty() {
                        if let Err(e) = self
                            .hardware
                            .storage
                            .write_wifi_credentials(connected_ssid, password)
                        {
                            warn!("Failed to save WiFi credentials: {}", e);
                        }
                    }
                    state.phase = WifiPhase::ConnectResult;
                    state.status_message = format!("已连接: {} ({})", connected_ssid, ip);
                }
                WifiStatus::Failed { reason, .. } => {
                    state.phase = WifiPhase::ConnectResult;
                    state.status_message = format!("连接失败: {}", reason);
                }
                WifiStatus::NotConfigured => {
                    state.phase = WifiPhase::ConnectResult;
                    state.status_message = "未配置凭据".to_string();
                }
            }
        }
        self.render_wifi_settings_page();
    }

    // ── WiFi rendering ─────────────────────────────────────────────

    pub(super) fn render_wifi_settings_page(&mut self) {
        let state = match &self.wifi_state {
            Some(s) => s.clone(),
            None => return,
        };

        match state.phase {
            WifiPhase::Scanning => self.render_wifi_status("正在扫描 WiFi...", ""),
            WifiPhase::Connecting => self.render_wifi_status(&state.status_message, ""),
            WifiPhase::NetworkList => self.render_wifi_network_list(&state),
            WifiPhase::PasswordChoice => self.render_wifi_password_choice(&state),
            WifiPhase::PasswordInput => self.render_wifi_password_input(&state),
            WifiPhase::ConnectResult => {
                self.render_wifi_status(&state.status_message, "任意键继续")
            }
        }
    }

    pub(super) fn render_wifi_network_list(&mut self, state: &WifiState) {
        self.display.clear(0xFF);

        let row_height = self.ui_font.glyph_height as usize + 10;
        let bottom_reserved = self.bottom_bar_total_height() + 6;
        let visible_rows = (Display::height() - super::CONTENT_TOP - bottom_reserved) / row_height;

        if state.scanned_aps.is_empty() {
            self.display.draw_text_wrapped(
                &self.ui_font,
                "未发现 WiFi 网络",
                super::LIST_X,
                super::CONTENT_TOP,
                Display::width() - super::LIST_RIGHT_MARGIN,
                3,
            );
        } else {
            let start = state.selected_index.saturating_sub(visible_rows / 2);
            let start = start.min(state.scanned_aps.len().saturating_sub(visible_rows));
            let end = (start + visible_rows).min(state.scanned_aps.len());

            for i in start..end {
                let row = i - start;
                let y = super::CONTENT_TOP + row * row_height;

                if i == state.selected_index {
                    self.display.fill_rect(12, y, 2, row_height - 4, 0x00);
                }

                let ap = &state.scanned_aps[i];
                let ssid: String = if ap.ssid.is_empty() {
                    "[隐藏网络]".to_string()
                } else {
                    ap.ssid.to_string()
                };
                let ssid =
                    truncate_for_width(&self.ui_font, &ssid, WIFI_SIGNAL_X_OFFSET - super::LIST_X);
                self.display
                    .draw_text_font(&self.ui_font, &ssid, super::LIST_X, y);

                self.draw_signal_bars(y, ap.signal_strength);

                let auth_text = match ap.auth_method {
                    None | Some(AuthMethod::None) => "(开放)",
                    _ => "",
                };
                if !auth_text.is_empty() {
                    let auth_x = WIFI_SIGNAL_X_OFFSET + 60;
                    self.display
                        .draw_text_font(&self.ui_font, auth_text, auth_x, y);
                }
            }
        }

        self.draw_bottom_bar(&state.status_message, "");
    }

    pub(super) fn render_wifi_password_choice(&mut self, state: &WifiState) {
        self.display.clear(0xFF);

        let ap = state
            .scanned_aps
            .get(state.selected_ap_index)
            .map(|ap| ap.ssid.clone())
            .unwrap_or_default();

        let row_height = self.ui_font.glyph_height as usize + 10;

        let options: Vec<String> = WIFI_PRESET_PASSWORDS
            .iter()
            .enumerate()
            .map(|(i, pw)| format!("常用密码 {} ({})", i + 1, pw))
            .chain(std::iter::once("手动输入...".to_string()))
            .collect();

        for (i, option) in options.iter().enumerate() {
            let y = super::CONTENT_TOP + 4 + i * row_height;
            if i == state.selected_index {
                self.display
                    .fill_rect(12, y, 2, self.ui_font.glyph_height as usize + 4, 0x00);
            }
            let label = truncate_for_width(
                &self.ui_font,
                option,
                Display::width() - super::LIST_X - super::LIST_RIGHT_MARGIN,
            );
            self.display
                .draw_text_font(&self.ui_font, &label, super::LIST_X, y);
        }

        let bottom_label = format!("选择密码: {}", ap);
        self.draw_bottom_bar(&bottom_label, "");
    }

    pub(super) fn render_wifi_password_input(&mut self, state: &WifiState) {
        self.display.clear(0xFF);

        let ap = state
            .scanned_aps
            .get(state.selected_ap_index)
            .map(|ap| ap.ssid.clone())
            .unwrap_or_default();

        let input_y = super::CONTENT_TOP + 12;

        // Render password characters with cursor highlight
        let max_display = 20; // max chars to show on screen
        let chars: Vec<char> = state.password_buf.chars().collect();
        let start = if chars.len() <= max_display {
            0
        } else {
            state.cursor_pos.saturating_sub(max_display / 2)
        };
        let start = start.min(chars.len().saturating_sub(max_display));
        let end = (start + max_display).min(chars.len());

        let cursor_x = super::LIST_X + 8; // indent
        let char_w = self.ui_font.glyph_width as usize;

        // Draw leading bracket
        self.display
            .draw_text_font(&self.ui_font, "[", super::LIST_X, input_y);

        // Draw visible characters
        for i in start..end {
            let x = cursor_x + (i - start) * (char_w + 2);
            let ch_str: String = if i < chars.len() {
                chars[i].to_string()
            } else {
                " ".to_string()
            };
            self.display
                .draw_text_font(&self.ui_font, &ch_str, x, input_y);

            if i == state.cursor_pos {
                // Underline cursor
                self.display.fill_rect(
                    x,
                    input_y + self.ui_font.glyph_height as usize + 1,
                    char_w,
                    2,
                    0x00,
                );
            }
        }

        // Draw trailing bracket and empty slots indicator
        let after_x = cursor_x + (end - start) * (char_w + 2);
        let remaining = state.password_buf.len().saturating_sub(end);
        if remaining > 0 {
            let dots = format!("...]");
            self.display
                .draw_text_font(&self.ui_font, &dots, after_x, input_y);
        } else if state.cursor_pos == state.password_buf.len() && state.password_buf.len() < 64 {
            // Show cursor underline at append position
            let cursor_end_x = cursor_x + (chars.len() - start) * (char_w + 2);
            self.display.fill_rect(
                cursor_end_x,
                input_y + self.ui_font.glyph_height as usize + 1,
                char_w,
                2,
                0x00,
            );
            let close_x = cursor_end_x + char_w + 2;
            self.display
                .draw_text_font(&self.ui_font, "]", close_x, input_y);
        } else {
            let close_x = after_x;
            if state.cursor_pos == state.password_buf.len() && state.password_buf.len() >= 64 {
                // Cursor at end but buffer full
                self.display.fill_rect(
                    close_x,
                    input_y + self.ui_font.glyph_height as usize + 1,
                    char_w,
                    2,
                    0x00,
                );
            }
            self.display
                .draw_text_font(&self.ui_font, "]", close_x, input_y);
        }

        // Character set preview row
        let preview_y = input_y + self.ui_font.glyph_height as usize + 16;
        self.display.fill_rect(
            super::LIST_X,
            preview_y - 4,
            Display::width() - super::LIST_X - super::LIST_RIGHT_MARGIN,
            1,
            0x00,
        );

        // Determine the current character being edited at cursor position
        let current_char = if state.cursor_pos < chars.len() {
            chars[state.cursor_pos]
        } else {
            '0' // appending: first char in charset (same as what Down would insert)
        };

        let charset: Vec<char> = WIFI_PASSWORD_CHARS.chars().collect();
        let current_idx = charset.iter().position(|&c| c == current_char).unwrap_or(0);

        // Show a window of charset centered on current character
        let preview_window = 18;
        let half = preview_window / 2;
        let cs_start = if charset.len() <= preview_window {
            0
        } else {
            current_idx
                .saturating_sub(half)
                .min(charset.len() - preview_window)
        };
        let cs_end = (cs_start + preview_window).min(charset.len());

        let mut preview_str = String::with_capacity(64);
        for (i, &c) in charset[cs_start..cs_end].iter().enumerate() {
            let global_idx = cs_start + i;
            if global_idx == current_idx {
                preview_str.push('[');
                preview_str.push(c);
                preview_str.push(']');
            } else {
                preview_str.push(' ');
                preview_str.push(c);
                preview_str.push(' ');
            }
        }

        let preview_label = "字符: ";
        self.display
            .draw_text_font(&self.ui_font, preview_label, super::LIST_X, preview_y);
        let label_w = self.ui_font.text_width(preview_label);
        self.display.draw_text_font(
            &self.ui_font,
            &preview_str,
            super::LIST_X + label_w,
            preview_y,
        );

        // Help text (condensed, placed above bottom bar)
        let help_y = preview_y + self.ui_font.glyph_height as usize + 8;
        self.display.draw_text_wrapped(
            &self.ui_font,
            "上下切换字符  左右移动光标  确认连接  返回取消",
            super::LIST_X,
            help_y,
            Display::width() - super::LIST_RIGHT_MARGIN,
            4,
        );

        let bottom_label = format!("连接到: {}", ap);
        self.draw_bottom_bar(&bottom_label, "");
    }

    pub(super) fn render_wifi_status(&mut self, message: &str, hint: &str) {
        self.display.clear(0xFF);

        let msg_y = Display::height() / 2 - self.ui_font.glyph_height as usize;
        let msg = truncate_for_width(&self.ui_font, message, Display::width() - 2 * super::LIST_X);
        let msg_w = self.ui_font.text_width(&msg);
        let msg_x = (Display::width() - msg_w) / 2;
        self.display
            .draw_text_font(&self.ui_font, &msg, msg_x, msg_y);

        self.draw_bottom_bar(hint, "");
    }

    pub(super) fn draw_signal_bars(&mut self, y: usize, signal_strength: i8) {
        // Convert dBm to bar count: >= -50 → 4, >= -65 → 3, >= -80 → 2, else → 1
        let bars = if signal_strength >= -50 {
            4
        } else if signal_strength >= -65 {
            3
        } else if signal_strength >= -80 {
            2
        } else {
            1
        };

        let bar_h = 4;
        let bar_w = 4;
        let bar_gap = 2;
        let base_y = y + self.ui_font.glyph_height as usize - 4;

        for i in 0..4 {
            let bx = WIFI_SIGNAL_X_OFFSET + i * (bar_w + bar_gap);
            let bh = bar_h * (i + 1);
            let by = base_y - bh;
            if i < bars {
                self.display.fill_rect(bx, by, bar_w, bh, 0x00);
            } else {
                // Outline for empty bars
                self.display.fill_rect(bx, by, bar_w, 1, 0x00);
                self.display.fill_rect(bx, by, 1, bh, 0x00);
                self.display.fill_rect(bx + bar_w - 1, by, 1, bh, 0x00);
                self.display.fill_rect(bx, by + bh - 1, bar_w, 1, 0x00);
            }
        }
    }
}
