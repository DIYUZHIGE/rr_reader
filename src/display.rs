mod controller;
mod drawing;
mod glyph_cache;
mod refresh;

pub use self::refresh::RefreshMode;

use self::glyph_cache::GlyphCache;
use anyhow::Result;
use esp_idf_hal::gpio::{Input, Output, PinDriver, Pull};
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::sys;
use log::info;
use std::ptr;

const PHYSICAL_DISPLAY_WIDTH: usize = 800;
const PHYSICAL_DISPLAY_HEIGHT: usize = 480;
const PHYSICAL_DISPLAY_WIDTH_BYTES: usize = PHYSICAL_DISPLAY_WIDTH / 8;
const BUFFER_SIZE: usize = PHYSICAL_DISPLAY_WIDTH_BYTES * PHYSICAL_DISPLAY_HEIGHT;

const DISPLAY_WIDTH: usize = PHYSICAL_DISPLAY_HEIGHT;
const DISPLAY_HEIGHT: usize = PHYSICAL_DISPLAY_WIDTH;

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

const BUSY_ACTIVE_HIGH: bool = true;
const INIT_BUSY_TIMEOUT_MS: u32 = 2_000;
const REFRESH_BUSY_TIMEOUT_MS: u32 = 30_000;
const SPI_WRITE_CHUNK: usize = 4096;

pub struct Display {
    spi_handle: sys::spi_device_handle_t,
    cs: PinDriver<'static, Output>,
    dc: PinDriver<'static, Output>,
    rst: PinDriver<'static, Output>,
    busy: PinDriver<'static, Input>,
    framebuffer: Vec<u8>,
    dirty: bool,
    initialized: bool,
    /// Whether RED RAM currently holds the same frame as framebuffer
    red_ram_synced: bool,
    /// Count of consecutive fast refreshes since last cleanup refresh
    fast_refresh_count: u32,
    glyph_cache: GlyphCache,
}

impl Display {
    /// Create display driver. The SPI bus must already be initialized
    /// (typically by sdspi_host_init for SD card sharing). This adds the
    /// display as a device on the shared SPI2 bus.
    pub fn new(peripherals: Peripherals) -> Result<Self> {
        // Add display as a device on the shared SPI2 bus (no CS; manual control).
        let device_config = sys::spi_device_interface_config_t {
            spics_io_num: -1,
            clock_speed_hz: 40_000_000,
            mode: 0,
            queue_size: 1,
            ..Default::default()
        };

        let mut handle: sys::spi_device_handle_t = ptr::null_mut();
        unsafe {
            sys::esp!(sys::spi_bus_add_device(
                sys::spi_host_device_t_SPI2_HOST,
                &device_config,
                &mut handle,
            ))?;
        }

        info!("Display SPI device added to shared bus (40 MHz)");

        Ok(Self {
            spi_handle: handle,
            cs: PinDriver::output(peripherals.pins.gpio21)?,
            dc: PinDriver::output(peripherals.pins.gpio4)?,
            rst: PinDriver::output(peripherals.pins.gpio5)?,
            busy: PinDriver::input(peripherals.pins.gpio6, Pull::Floating)?,
            framebuffer: vec![0xFF; BUFFER_SIZE],
            dirty: false,
            initialized: false,
            red_ram_synced: false,
            fast_refresh_count: 0,
            glyph_cache: GlyphCache::new(),
        })
    }

    pub fn begin(&mut self) -> Result<()> {
        info!("Initializing X4 SSD1677 e-ink display (40 MHz SPI, shared bus)");
        self.configure_gpio()?;
        self.cs.set_high()?;
        self.dc.set_high()?;
        self.reset_display()?;
        info!("Display reset complete");
        self.init_controller()?;
        info!("Display controller init complete");
        self.initialized = true;
        self.red_ram_synced = true;
        Ok(())
    }

    pub fn width() -> usize {
        DISPLAY_WIDTH
    }

    pub fn height() -> usize {
        DISPLAY_HEIGHT
    }
}
