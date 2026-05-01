use super::{
    Display, BUSY_ACTIVE_HIGH, CMD_AUTO_WRITE_BW_RAM, CMD_AUTO_WRITE_RED_RAM,
    CMD_BOOSTER_SOFT_START, CMD_BORDER_WAVEFORM, CMD_DATA_ENTRY_MODE, CMD_DRIVER_OUTPUT_CONTROL,
    CMD_SET_RAM_X_COUNTER, CMD_SET_RAM_X_RANGE, CMD_SET_RAM_Y_COUNTER, CMD_SET_RAM_Y_RANGE,
    CMD_SOFT_RESET, CMD_TEMP_SENSOR_CONTROL, DISPLAY_HEIGHT, DISPLAY_WIDTH, INIT_BUSY_TIMEOUT_MS,
    SPI_WRITE_CHUNK,
};
use anyhow::{anyhow, Result};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::sys;
use log::{debug, info, warn};

impl Display {
    pub(super) fn reset_display(&mut self) -> Result<()> {
        self.rst.set_high()?;
        FreeRtos::delay_ms(20);
        self.rst.set_low()?;
        FreeRtos::delay_ms(2);
        self.rst.set_high()?;
        FreeRtos::delay_ms(20);
        Ok(())
    }

    pub(super) fn init_controller(&mut self) -> Result<()> {
        info!(
            "Display init: sending soft reset command (0x{:02X})",
            CMD_SOFT_RESET
        );
        self.send_command(CMD_SOFT_RESET)?;
        info!("Display init: soft reset command sent, waiting for busy...");

        // SSD1677 datasheet: BUSY rises within 100us after reset command.
        // Give a small safety margin before polling.
        FreeRtos::delay_ms(5);
        let mut no_poll = None;
        self.wait_while_busy("soft reset", INIT_BUSY_TIMEOUT_MS, &mut no_poll)?;
        info!("Display init: soft reset complete");

        info!("Display init: temperature sensor (internal)");
        self.send_command(CMD_TEMP_SENSOR_CONTROL)?;
        self.send_data_byte(0x80)?;
        info!("Display init: temperature sensor done");

        info!("Display init: booster soft start");
        self.send_command(CMD_BOOSTER_SOFT_START)?;
        self.send_data(&[0xAE, 0xC7, 0xC3, 0xC0, 0x40])?;
        info!("Display init: booster soft start done");

        info!("Display init: driver output control");
        self.send_command(CMD_DRIVER_OUTPUT_CONTROL)?;
        let height_minus_one = (DISPLAY_HEIGHT - 1) as u16;
        self.send_data(&[
            (height_minus_one & 0xFF) as u8,
            (height_minus_one >> 8) as u8,
            0x02, // SM=1: interlaced gate mode
        ])?;
        info!("Display init: driver output control done");

        self.send_command(CMD_BORDER_WAVEFORM)?;
        self.send_data_byte(0x01)?;
        info!("Display init: border waveform done");

        info!("Display init: RAM area");
        self.set_ram_area(0, 0, DISPLAY_WIDTH as u16, DISPLAY_HEIGHT as u16)?;
        info!("Display init: RAM area done");

        info!("Display init: clear BW RAM");
        self.send_command(CMD_AUTO_WRITE_BW_RAM)?;
        self.send_data_byte(0xF7)?; // White pattern for all pixels
        let mut no_poll = None;
        self.wait_while_busy("auto write BW RAM", INIT_BUSY_TIMEOUT_MS, &mut no_poll)?;
        info!("Display init: clear BW RAM done");

        info!("Display init: clear RED RAM");
        self.send_command(CMD_AUTO_WRITE_RED_RAM)?;
        self.send_data_byte(0xF7)?;
        let mut no_poll = None;
        self.wait_while_busy("auto write RED RAM", INIT_BUSY_TIMEOUT_MS, &mut no_poll)?;
        info!("Display init: clear RED RAM done");

        Ok(())
    }

    pub(super) fn set_ram_area(&mut self, x: u16, y: u16, w: u16, h: u16) -> Result<()> {
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

    pub(super) fn write_framebuffer(&mut self, command: u8) -> Result<()> {
        self.send_command(command)?;
        self.dc.set_high()?;
        self.cs.set_low()?;

        let handle = self.spi_handle;
        let mut result = Ok(());
        for chunk in self.framebuffer.chunks(SPI_WRITE_CHUNK) {
            if let Err(e) = Self::spi_transmit_handle(handle, chunk) {
                result = Err(e);
                break;
            }
        }

        let cs_result = self.cs.set_high();
        result?;
        cs_result?;
        Ok(())
    }

    fn spi_transmit(&mut self, data: &[u8]) -> Result<()> {
        Self::spi_transmit_handle(self.spi_handle, data)
    }

    fn spi_transmit_handle(handle: sys::spi_device_handle_t, data: &[u8]) -> Result<()> {
        let mut trans: sys::spi_transaction_t = unsafe { std::mem::zeroed() };
        trans.length = data.len() * 8;
        trans.__bindgen_anon_1.tx_buffer = data.as_ptr() as *const _;
        unsafe {
            sys::esp!(sys::spi_device_transmit(handle, &mut trans))?;
        }
        Ok(())
    }

    pub(super) fn send_command(&mut self, command: u8) -> Result<()> {
        self.dc.set_low()?;
        self.cs.set_low()?;
        self.spi_transmit(&[command])?;
        self.cs.set_high()?;
        self.dc.set_high()?;
        Ok(())
    }

    pub(super) fn send_data_byte(&mut self, data: u8) -> Result<()> {
        self.send_data(&[data])
    }

    pub(super) fn send_data(&mut self, data: &[u8]) -> Result<()> {
        self.dc.set_high()?;
        self.cs.set_low()?;
        let mut result = Ok(());
        for chunk in data.chunks(SPI_WRITE_CHUNK) {
            if let Err(e) = self.spi_transmit(chunk) {
                result = Err(e);
                break;
            }
        }
        let cs_result = self.cs.set_high();
        result?;
        cs_result?;
        Ok(())
    }

    pub(super) fn wait_while_busy(
        &self,
        label: &str,
        timeout_ms: u32,
        poll: &mut Option<&mut dyn FnMut()>,
    ) -> Result<()> {
        let mut elapsed_ms = 0u32;
        let initial_busy = self.is_busy();
        debug!(
            "Display wait: {label}; initial_busy={initial_busy}; active_high={BUSY_ACTIVE_HIGH}"
        );

        while self.is_busy() && elapsed_ms < timeout_ms {
            if let Some(poll) = poll.as_deref_mut() {
                poll();
            }
            FreeRtos::delay_ms(1);
            elapsed_ms += 1;
            // Periodically report stuck state for diagnosis
            if elapsed_ms % 500 == 0 {
                debug!(
                    "Display wait: {label} ({elapsed_ms} ms); still busy={}",
                    self.is_busy()
                );
            }
        }

        if elapsed_ms >= timeout_ms {
            warn!(
                "Display wait timed out: {label}; still busy={}",
                self.is_busy()
            );
            return Err(anyhow!("display wait timed out: {label}"));
        } else {
            debug!(
                "Display wait done: {label} ({elapsed_ms} ms); busy={}",
                self.is_busy()
            );
        }

        Ok(())
    }

    fn is_busy(&self) -> bool {
        if BUSY_ACTIVE_HIGH {
            self.busy.is_high()
        } else {
            self.busy.is_low()
        }
    }

    pub(super) fn configure_gpio(&self) -> Result<()> {
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
}
