use anyhow::Result;
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{AnyInputPin, AnyOutputPin, Input, Output, PinDriver, Pull};
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::spi::{config, Dma, SpiDeviceDriver, SpiDriver, SpiDriverConfig};
use esp_idf_hal::sys;
use esp_idf_hal::units::*;
use log::{debug, info, warn};
use std::ptr;

const DISPLAY_WIDTH: usize = 800;
const DISPLAY_HEIGHT: usize = 480;
const DISPLAY_WIDTH_BYTES: usize = DISPLAY_WIDTH / 8;
const BUFFER_SIZE: usize = DISPLAY_WIDTH_BYTES * DISPLAY_HEIGHT;

// SSD1677 commands
const CMD_SOFT_RESET: u8 = 0x12;
const CMD_TEMP_SENSOR_CONTROL: u8 = 0x18;
const CMD_BOOSTER_SOFT_START: u8 = 0x0C;
const CMD_DRIVER_OUTPUT_CONTROL: u8 = 0x01;
const CMD_BORDER_WAVEFORM: u8 = 0x3C;
const CMD_DATA_ENTRY_MODE: u8 = 0x11;
const CMD_SET_RAM_X_RANGE: u8 = 0x44;
const CMD_SET_RAM_Y_RANGE: u8 = 0x45;
const CMD_SET_RAM_X_COUNTER: u8 = 0x4E;
const CMD_SET_RAM_Y_COUNTER: u8 = 0x4F;
const CMD_WRITE_RAM_BW: u8 = 0x24;
const CMD_WRITE_RAM_RED: u8 = 0x26;
const CMD_AUTO_WRITE_BW_RAM: u8 = 0x46;
const CMD_AUTO_WRITE_RED_RAM: u8 = 0x47;
const CMD_DISPLAY_UPDATE_CTRL1: u8 = 0x21;
const CMD_DISPLAY_UPDATE_CTRL2: u8 = 0x22;
const CMD_MASTER_ACTIVATION: u8 = 0x20;
const CMD_WRITE_TEMPERATURE: u8 = 0x1A;

// Display Update Control 1 flags
const CTRL1_NORMAL: u8 = 0x00; // Use RED RAM as previous frame for differential
const CTRL1_BYPASS_RED: u8 = 0x40; // Treat RED RAM as all-white

// Display Update Control 2 flags
const CTRL2_CLOCK: u8 = 0x80;
const CTRL2_ANALOG: u8 = 0x40;
const CTRL2_TEMP: u8 = 0x20;
const CTRL2_LUT: u8 = 0x10;
const CTRL2_MODE: u8 = 0x08;
const CTRL2_DISPLAY: u8 = 0x04;

// Full: CLOCK + ANALOG + TEMP + LUT + DISPLAY
const CTRL2_FULL: u8 = CTRL2_CLOCK | CTRL2_ANALOG | CTRL2_TEMP | CTRL2_LUT | CTRL2_DISPLAY; // 0xF4
// Half: CLOCK + ANALOG + LUT + DISPLAY (skip temperature load)
const CTRL2_HALF: u8 = CTRL2_CLOCK | CTRL2_ANALOG | CTRL2_LUT | CTRL2_DISPLAY; // 0xD4
// Fast: LUT + MODE + DISPLAY (differential, uses internal temperature)
const CTRL2_FAST: u8 = CTRL2_LUT | CTRL2_MODE | CTRL2_DISPLAY; // 0x1C

// High temperature value to accelerate refresh in half mode
const HALF_TEMPERATURE: u8 = 0x5A;
// Insert a full refresh every N fast refreshes to clear ghosting
const FAST_REFRESH_LIMIT: u32 = 20;

const BUSY_ACTIVE_HIGH: bool = true;
const INIT_BUSY_TIMEOUT_MS: u32 = 2_000;
const REFRESH_BUSY_TIMEOUT_MS: u32 = 30_000;
const SPI_WRITE_CHUNK: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshMode {
    /// Full waveform, best quality, ~1600ms. Use for boot, wakeup, ghosting cleanup.
    Full,
    /// Skip temperature load, ~900ms. Use for menu navigation.
    Half,
    /// Differential update, only changed pixels, ~400-600ms. Use for page turns.
    Fast,
}

pub struct Display {
    spi: SpiDeviceDriver<'static, SpiDriver<'static>>,
    cs: PinDriver<'static, Output>,
    dc: PinDriver<'static, Output>,
    rst: PinDriver<'static, Output>,
    busy: PinDriver<'static, Input>,
    framebuffer: Vec<u8>,
    dirty: bool,
    initialized: bool,
    /// Whether RED RAM currently holds the same frame as framebuffer
    red_ram_synced: bool,
    /// Count of consecutive fast refreshes since last full refresh
    fast_refresh_count: u32,
}

impl Display {
    pub fn new(peripherals: Peripherals) -> Result<Self> {
        let spi = SpiDeviceDriver::new_single(
            peripherals.spi2,
            peripherals.pins.gpio8,
            peripherals.pins.gpio10,
            None::<AnyInputPin>,
            None::<AnyOutputPin>,
            &SpiDriverConfig::new().dma(Dma::Auto(SPI_WRITE_CHUNK)),
            &config::Config::new()
                .baudrate(40.MHz().into())
                .write_only(true)
                .polling(true),
        )?;

        Ok(Self {
            spi,
            cs: PinDriver::output(peripherals.pins.gpio21)?,
            dc: PinDriver::output(peripherals.pins.gpio4)?,
            rst: PinDriver::output(peripherals.pins.gpio5)?,
            busy: PinDriver::input(peripherals.pins.gpio6, Pull::Floating)?,
            framebuffer: vec![0xFF; BUFFER_SIZE],
            dirty: false,
            initialized: false,
            red_ram_synced: false,
            fast_refresh_count: 0,
        })
    }

    // ── public API ──────────────────────────────────────────────

    pub fn begin(&mut self) -> Result<()> {
        info!("Initializing X4 SSD1677 e-ink display (40 MHz SPI)");
        self.configure_gpio()?;
        self.dump_gpio_config();
        self.cs.set_high()?;
        self.dc.set_high()?;
        self.reset_display()?;
        info!("Display reset complete");
        self.init_controller()?;
        info!("Display controller init complete");
        self.initialized = true;
        self.red_ram_synced = true; // Both RAM cleared to white in init
        Ok(())
    }

    pub fn show_boot_screen(&mut self) -> Result<()> {
        info!("Drawing boot screen");
        self.clear(0xFF);
        self.draw_text("rr_reader", 220, 210, 7);
        self.dirty = true;
        Ok(())
    }

    /// Flush the dirty framebuffer to the display. First display after init
    /// always uses Full refresh. After that uses Fast refresh with automatic
    /// Full insertion every N refreshes to clear ghosting.
    pub fn flush_if_dirty(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        let mode = if !self.red_ram_synced {
            RefreshMode::Full
        } else if self.fast_refresh_count >= FAST_REFRESH_LIMIT {
            debug!("Inserting periodic full refresh after {} fast refreshes", self.fast_refresh_count);
            RefreshMode::Full
        } else {
            RefreshMode::Fast
        };

        self.refresh(mode)?;
        self.dirty = false;
        Ok(())
    }

    /// Flush with an explicit refresh mode.
    pub fn flush_with_mode(&mut self, mode: RefreshMode) -> Result<()> {
        if self.dirty {
            self.refresh(mode)?;
            self.dirty = false;
        }
        Ok(())
    }

    /// Force a full refresh to clear ghosting artifacts.
    pub fn force_full_refresh(&mut self) -> Result<()> {
        info!("Forced full refresh for ghosting cleanup");
        self.refresh(RefreshMode::Full)
    }

    pub fn clear(&mut self, color: u8) {
        self.framebuffer.fill(color);
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn draw_text(&mut self, text: &str, x: usize, y: usize, scale: usize) {
        let mut cursor_x = x;
        for ch in text.chars() {
            self.draw_char(ch, cursor_x, y, scale);
            cursor_x += 6 * scale;
        }
        self.dirty = true;
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    pub fn framebuffer_mut(&mut self) -> &mut [u8] {
        &mut self.framebuffer
    }

    pub fn width() -> usize {
        DISPLAY_WIDTH
    }

    pub fn height() -> usize {
        DISPLAY_HEIGHT
    }

    // ── refresh modes ───────────────────────────────────────────

    fn refresh(&mut self, mode: RefreshMode) -> Result<()> {
        if !self.initialized {
            self.begin()?;
        }

        info!("Display refresh: {:?}", mode);
        self.set_ram_area(0, 0, DISPLAY_WIDTH as u16, DISPLAY_HEIGHT as u16)?;

        let fb = self.framebuffer.clone();

        match mode {
            RefreshMode::Full => self.refresh_full(&fb)?,
            RefreshMode::Half => self.refresh_half(&fb)?,
            RefreshMode::Fast => self.refresh_fast(&fb)?,
        }

        Ok(())
    }

    /// Full refresh: write both RAM, bypass RED, full waveform.
    fn refresh_full(&mut self, fb: &[u8]) -> Result<()> {
        self.write_ram_buffer(CMD_WRITE_RAM_BW, fb)?;
        self.write_ram_buffer(CMD_WRITE_RAM_RED, fb)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL1)?;
        self.send_data_byte(CTRL1_BYPASS_RED)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL2)?;
        self.send_data_byte(CTRL2_FULL)?;

        self.send_command(CMD_MASTER_ACTIVATION)?;
        self.wait_while_busy("full refresh", REFRESH_BUSY_TIMEOUT_MS);

        self.red_ram_synced = true;
        self.fast_refresh_count = 0;
        info!("Full refresh complete");
        Ok(())
    }

    /// Half refresh: write both RAM, inject high temperature, skip temp load.
    fn refresh_half(&mut self, fb: &[u8]) -> Result<()> {
        self.write_ram_buffer(CMD_WRITE_RAM_BW, fb)?;
        self.write_ram_buffer(CMD_WRITE_RAM_RED, fb)?;

        // Inject high temperature to accelerate response
        self.send_command(CMD_WRITE_TEMPERATURE)?;
        self.send_data_byte(HALF_TEMPERATURE)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL1)?;
        self.send_data_byte(CTRL1_BYPASS_RED)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL2)?;
        self.send_data_byte(CTRL2_HALF)?;

        self.send_command(CMD_MASTER_ACTIVATION)?;
        self.wait_while_busy("half refresh", REFRESH_BUSY_TIMEOUT_MS);

        self.red_ram_synced = true;
        self.fast_refresh_count = 0;
        info!("Half refresh complete");
        Ok(())
    }

    /// Fast (differential) refresh: write only BW RAM, compare against RED RAM.
    /// Only pixels that differ from the previous frame are driven.
    fn refresh_fast(&mut self, fb: &[u8]) -> Result<()> {
        // Write new frame to BW RAM only. RED RAM keeps the previous frame
        // so the controller can compute the delta.
        self.write_ram_buffer(CMD_WRITE_RAM_BW, fb)?;
        // Do NOT write RED RAM — keep the old frame for differential compare.

        // Normal mode: controller compares BW vs RED, only drives changed pixels
        self.send_command(CMD_DISPLAY_UPDATE_CTRL1)?;
        self.send_data_byte(CTRL1_NORMAL)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL2)?;
        self.send_data_byte(CTRL2_FAST)?;

        self.send_command(CMD_MASTER_ACTIVATION)?;
        self.wait_while_busy("fast refresh", REFRESH_BUSY_TIMEOUT_MS);

        // After display, copy new frame to RED RAM as baseline for next differential
        self.write_ram_buffer(CMD_WRITE_RAM_RED, fb)?;

        self.red_ram_synced = true;
        self.fast_refresh_count += 1;

        info!("Fast refresh complete (count: {})", self.fast_refresh_count);
        Ok(())
    }

    // ── controller init ─────────────────────────────────────────

    fn reset_display(&mut self) -> Result<()> {
        self.rst.set_high()?;
        FreeRtos::delay_ms(20);
        self.rst.set_low()?;
        FreeRtos::delay_ms(2);
        self.rst.set_high()?;
        FreeRtos::delay_ms(20);
        Ok(())
    }

    fn init_controller(&mut self) -> Result<()> {
        info!("Display init: soft reset");
        self.send_command(CMD_SOFT_RESET)?;
        self.wait_while_busy("soft reset", INIT_BUSY_TIMEOUT_MS);

        info!("Display init: temperature sensor (internal)");
        self.send_command(CMD_TEMP_SENSOR_CONTROL)?;
        self.send_data_byte(0x80)?;

        info!("Display init: booster soft start");
        self.send_command(CMD_BOOSTER_SOFT_START)?;
        self.send_data(&[0xAE, 0xC7, 0xC3, 0xC0, 0x40])?;

        info!("Display init: driver output control");
        self.send_command(CMD_DRIVER_OUTPUT_CONTROL)?;
        let height_minus_one = (DISPLAY_HEIGHT - 1) as u16;
        self.send_data(&[
            (height_minus_one & 0xFF) as u8,
            (height_minus_one >> 8) as u8,
            0x02, // SM=1: interlaced gate mode
        ])?;

        self.send_command(CMD_BORDER_WAVEFORM)?;
        self.send_data_byte(0x01)?;

        info!("Display init: RAM area");
        self.set_ram_area(0, 0, DISPLAY_WIDTH as u16, DISPLAY_HEIGHT as u16)?;

        info!("Display init: clear BW RAM");
        self.send_command(CMD_AUTO_WRITE_BW_RAM)?;
        self.send_data_byte(0xF7)?; // White pattern for all pixels
        self.wait_while_busy("auto write BW RAM", INIT_BUSY_TIMEOUT_MS);

        info!("Display init: clear RED RAM");
        self.send_command(CMD_AUTO_WRITE_RED_RAM)?;
        self.send_data_byte(0xF7)?;
        self.wait_while_busy("auto write RED RAM", INIT_BUSY_TIMEOUT_MS);

        Ok(())
    }

    // ── RAM helpers ─────────────────────────────────────────────

    fn set_ram_area(&mut self, x: u16, y: u16, w: u16, h: u16) -> Result<()> {
        // Y coordinate is reversed due to gate driver orientation
        let reversed_y = DISPLAY_HEIGHT as u16 - y - h;
        let x_end = x + w - 1;
        let y_start = reversed_y + h - 1;

        self.send_command(CMD_DATA_ENTRY_MODE)?;
        self.send_data_byte(0x01)?; // X increment, Y decrement

        self.send_command(CMD_SET_RAM_X_RANGE)?;
        self.send_data(&[
            (x & 0xFF) as u8,
            (x >> 8) as u8,
            (x_end & 0xFF) as u8,
            (x_end >> 8) as u8,
        ])?;

        self.send_command(CMD_SET_RAM_Y_RANGE)?;
        self.send_data(&[
            (y_start & 0xFF) as u8,
            (y_start >> 8) as u8,
            (reversed_y & 0xFF) as u8,
            (reversed_y >> 8) as u8,
        ])?;

        self.send_command(CMD_SET_RAM_X_COUNTER)?;
        self.send_data(&[(x & 0xFF) as u8, (x >> 8) as u8])?;

        self.send_command(CMD_SET_RAM_Y_COUNTER)?;
        self.send_data(&[(y_start & 0xFF) as u8, (y_start >> 8) as u8])?;
        Ok(())
    }

    fn write_ram_buffer(&mut self, command: u8, data: &[u8]) -> Result<()> {
        self.send_command(command)?;
        self.send_data(data)?;
        Ok(())
    }

    // ── SPI helpers ─────────────────────────────────────────────

    fn send_command(&mut self, command: u8) -> Result<()> {
        self.dc.set_low()?;
        self.cs.set_low()?;
        self.spi.write(&[command])?;
        self.cs.set_high()?;
        self.dc.set_high()?;
        Ok(())
    }

    fn send_data_byte(&mut self, data: u8) -> Result<()> {
        self.send_data(&[data])
    }

    fn send_data(&mut self, data: &[u8]) -> Result<()> {
        self.dc.set_high()?;
        self.cs.set_low()?;
        for chunk in data.chunks(SPI_WRITE_CHUNK) {
            self.spi.write(chunk)?;
        }
        self.cs.set_high()?;
        Ok(())
    }

    // ── busy polling ────────────────────────────────────────────

    fn wait_while_busy(&self, label: &str, timeout_ms: u32) {
        let mut elapsed_ms = 0u32;
        let initial_busy = self.is_busy();
        debug!(
            "Display wait: {label}; busy={initial_busy}; active_high={BUSY_ACTIVE_HIGH}"
        );

        while self.is_busy() && elapsed_ms < timeout_ms {
            FreeRtos::delay_ms(1);
            elapsed_ms += 1;
        }

        if elapsed_ms >= timeout_ms {
            warn!("Display wait timed out: {label}; still busy={}", self.is_busy());
        } else {
            debug!(
                "Display wait done: {label} ({elapsed_ms} ms); busy={}",
                self.is_busy()
            );
        }
    }

    fn is_busy(&self) -> bool {
        if BUSY_ACTIVE_HIGH {
            self.busy.is_high()
        } else {
            self.busy.is_low()
        }
    }

    // ── GPIO config ─────────────────────────────────────────────

    fn configure_gpio(&self) -> Result<()> {
        let output_mask = (1_u64 << 4) | (1_u64 << 5) | (1_u64 << 21);
        let input_mask = 1_u64 << 6;

        unsafe {
            sys::gpio_deep_sleep_hold_dis();
            for pin in [4, 5, 6, 8, 10, 21] {
                let _ = sys::gpio_hold_dis(pin);
            }

            let output_config = sys::gpio_config_t {
                pin_bit_mask: output_mask,
                mode: sys::gpio_mode_t_GPIO_MODE_OUTPUT,
                pull_up_en: sys::gpio_pullup_t_GPIO_PULLUP_DISABLE,
                pull_down_en: sys::gpio_pulldown_t_GPIO_PULLDOWN_DISABLE,
                intr_type: sys::gpio_int_type_t_GPIO_INTR_DISABLE,
            };
            sys::esp!(sys::gpio_config(&output_config))?;

            let input_config = sys::gpio_config_t {
                pin_bit_mask: input_mask,
                mode: sys::gpio_mode_t_GPIO_MODE_INPUT,
                pull_up_en: sys::gpio_pullup_t_GPIO_PULLUP_DISABLE,
                pull_down_en: sys::gpio_pulldown_t_GPIO_PULLDOWN_DISABLE,
                intr_type: sys::gpio_int_type_t_GPIO_INTR_DISABLE,
            };
            sys::esp!(sys::gpio_config(&input_config))?;
        }

        Ok(())
    }

    fn dump_gpio_config(&self) {
        let mask = (1_u64 << 4)
            | (1_u64 << 5)
            | (1_u64 << 6)
            | (1_u64 << 8)
            | (1_u64 << 10)
            | (1_u64 << 21);
        unsafe {
            let _ = sys::gpio_dump_io_configuration(ptr::null_mut(), mask);
        }
    }

    // ── basic drawing ───────────────────────────────────────────

    fn draw_char(&mut self, ch: char, x: usize, y: usize, scale: usize) {
        let glyph = glyph_5x7(ch);
        for (col, bits) in glyph.iter().enumerate() {
            for row in 0..7 {
                if (bits >> row) & 1 == 1 {
                    self.fill_rect(x + col * scale, y + row * scale, scale, scale, false);
                }
            }
        }
    }

    fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, white: bool) {
        for py in y..(y + h).min(DISPLAY_HEIGHT) {
            for px in x..(x + w).min(DISPLAY_WIDTH) {
                self.set_pixel(px, py, white);
            }
        }
    }

    fn set_pixel(&mut self, x: usize, y: usize, white: bool) {
        if x >= DISPLAY_WIDTH || y >= DISPLAY_HEIGHT {
            return;
        }

        let index = y * DISPLAY_WIDTH_BYTES + x / 8;
        let mask = 0x80 >> (x % 8);
        if white {
            self.framebuffer[index] |= mask;
        } else {
            self.framebuffer[index] &= !mask;
        }
    }
}

fn glyph_5x7(ch: char) -> [u8; 5] {
    match ch {
        'r' => [0x7C, 0x08, 0x04, 0x04, 0x08],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00],
        'a' => [0x38, 0x44, 0x7C, 0x44, 0x44],
        'd' => [0x38, 0x44, 0x44, 0x48, 0x7F],
        'e' => [0x38, 0x54, 0x54, 0x54, 0x18],
        'H' => [0x7F, 0x08, 0x08, 0x08, 0x7F],
        'l' => [0x00, 0x41, 0x7F, 0x40, 0x00],
        'o' => [0x38, 0x44, 0x44, 0x44, 0x38],
        'w' => [0x7C, 0x20, 0x18, 0x20, 0x7C],
        ' ' => [0x00, 0x00, 0x00, 0x00, 0x00],
        _ => [0x7F, 0x41, 0x5D, 0x41, 0x7F],
    }
}
