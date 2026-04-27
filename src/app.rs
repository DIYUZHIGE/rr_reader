use crate::display::Display;
use crate::hardware::Hardware;
use anyhow::Result;
use log::{debug, info};

const DEFAULT_LOOP_DELAY_MS: u32 = 10;
const IDLE_LOOP_DELAY_MS: u32 = 50;

pub struct ReaderApp {
    hardware: Hardware,
    display: Display,
    idle_ticks: u32,
}

impl ReaderApp {
    pub fn boot(mut hardware: Hardware, mut display: Display) -> Result<Self> {
        info!("Booting reader app");

        hardware.mount_storage()?;
        display.begin()?;
        display.show_boot_screen()?;

        Ok(Self {
            hardware,
            display,
            idle_ticks: 0,
        })
    }

    pub fn tick(&mut self) -> Result<()> {
        self.hardware.update_inputs()?;

        if self.hardware.has_user_activity() {
            self.idle_ticks = 0;
            debug!("User activity detected");
        } else {
            self.idle_ticks = self.idle_ticks.saturating_add(1);
        }

        self.display.flush_if_dirty()?;
        Ok(())
    }

    pub fn loop_delay_ms(&self) -> u32 {
        if self.idle_ticks > 500 {
            IDLE_LOOP_DELAY_MS
        } else {
            DEFAULT_LOOP_DELAY_MS
        }
    }
}
