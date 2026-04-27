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

const CTRL1_BYPASS_RED: u8 = 0x40;
const BUSY_ACTIVE_HIGH: bool = true;
const INIT_BUSY_TIMEOUT_MS: u32 = 2_000;
const REFRESH_BUSY_TIMEOUT_MS: u32 = 30_000;
const SPI_WRITE_CHUNK: usize = 4096;

pub struct Display {
    spi: SpiDeviceDriver<'static, SpiDriver<'static>>,
    cs: PinDriver<'static, Output>,
    dc: PinDriver<'static, Output>,
    rst: PinDriver<'static, Output>,
    busy: PinDriver<'static, Input>,
    framebuffer: Vec<u8>,
    dirty: bool,
    initialized: bool,
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
                .baudrate(10.MHz().into())
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
        })
    }

    pub fn begin(&mut self) -> Result<()> {
        info!("Initializing X4 SSD1677 e-ink display");
        self.configure_gpio()?;
        self.dump_gpio_config();
        self.cs.set_high()?;
        self.dc.set_high()?;
        self.reset_display()?;
        info!("Display reset complete");
        self.init_controller()?;
        info!("Display controller init complete");
        self.initialized = true;
        Ok(())
    }

    pub fn show_boot_screen(&mut self) -> Result<()> {
        info!("Drawing Hello world boot screen");
        self.clear(0xFF);
        self.draw_text("Hello world", 190, 210, 7);
        self.dirty = true;
        Ok(())
    }

    pub fn flush_if_dirty(&mut self) -> Result<()> {
        if self.dirty {
            self.display_full_refresh()?;
            self.dirty = false;
        }

        Ok(())
    }

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

        info!("Display init: temperature sensor");
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
            0x02,
        ])?;

        self.send_command(CMD_BORDER_WAVEFORM)?;
        self.send_data_byte(0x01)?;

        info!("Display init: RAM area");
        self.set_ram_area(0, 0, DISPLAY_WIDTH as u16, DISPLAY_HEIGHT as u16)?;

        info!("Display init: clear BW RAM");
        self.send_command(CMD_AUTO_WRITE_BW_RAM)?;
        self.send_data_byte(0xF7)?;
        self.wait_while_busy("auto write BW RAM", INIT_BUSY_TIMEOUT_MS);

        info!("Display init: clear RED RAM");
        self.send_command(CMD_AUTO_WRITE_RED_RAM)?;
        self.send_data_byte(0xF7)?;
        self.wait_while_busy("auto write RED RAM", INIT_BUSY_TIMEOUT_MS);

        Ok(())
    }

    fn set_ram_area(&mut self, x: u16, y: u16, w: u16, h: u16) -> Result<()> {
        let reversed_y = DISPLAY_HEIGHT as u16 - y - h;
        let x_end = x + w - 1;
        let y_start = reversed_y + h - 1;

        self.send_command(CMD_DATA_ENTRY_MODE)?;
        self.send_data_byte(0x01)?;

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

    fn display_full_refresh(&mut self) -> Result<()> {
        if !self.initialized {
            self.begin()?;
        }

        info!("Refreshing e-ink display with Hello world");
        self.set_ram_area(0, 0, DISPLAY_WIDTH as u16, DISPLAY_HEIGHT as u16)?;

        let framebuffer = self.framebuffer.clone();
        self.write_ram_buffer(CMD_WRITE_RAM_BW, &framebuffer)?;
        self.write_ram_buffer(CMD_WRITE_RAM_RED, &framebuffer)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL1)?;
        self.send_data_byte(CTRL1_BYPASS_RED)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL2)?;
        self.send_data_byte(0xF4)?;

        self.send_command(CMD_MASTER_ACTIVATION)?;
        self.wait_while_busy("full refresh", REFRESH_BUSY_TIMEOUT_MS);
        info!("Display full refresh complete");
        Ok(())
    }

    fn write_ram_buffer(&mut self, command: u8, data: &[u8]) -> Result<()> {
        self.send_command(command)?;
        self.send_data(data)?;
        Ok(())
    }

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

    fn wait_while_busy(&self, label: &str, timeout_ms: u32) {
        let mut elapsed_ms = 0u32;
        let initial_busy = self.is_busy();
        debug!(
            "Display wait start: {label}; busy={initial_busy}; active_high={BUSY_ACTIVE_HIGH}"
        );

        while self.is_busy() && elapsed_ms < timeout_ms {
            FreeRtos::delay_ms(1);
            elapsed_ms += 1;
        }

        if elapsed_ms >= timeout_ms {
            warn!("Display wait timed out: {label}; busy={}", self.is_busy());
        } else {
            debug!(
                "Display wait complete: {label} ({elapsed_ms} ms); busy={}",
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

    fn clear(&mut self, color: u8) {
        self.framebuffer.fill(color);
    }

    fn draw_text(&mut self, text: &str, x: usize, y: usize, scale: usize) {
        let mut cursor_x = x;
        for ch in text.chars() {
            self.draw_char(ch, cursor_x, y, scale);
            cursor_x += 6 * scale;
        }
    }

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
        'H' => [0x7F, 0x08, 0x08, 0x08, 0x7F],
        'e' => [0x38, 0x54, 0x54, 0x54, 0x18],
        'l' => [0x00, 0x41, 0x7F, 0x40, 0x00],
        'o' => [0x38, 0x44, 0x44, 0x44, 0x38],
        'w' => [0x7C, 0x20, 0x18, 0x20, 0x7C],
        'r' => [0x7C, 0x08, 0x04, 0x04, 0x08],
        'd' => [0x38, 0x44, 0x44, 0x48, 0x7F],
        ' ' => [0x00, 0x00, 0x00, 0x00, 0x00],
        _ => [0x7F, 0x41, 0x5D, 0x41, 0x7F],
    }
}
