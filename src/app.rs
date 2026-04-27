use crate::display::{Display, RefreshMode};
use crate::hardware::Hardware;
use crate::input::Button;
use crate::power::PowerManager;
use anyhow::Result;
use log::info;

const DEFAULT_LOOP_DELAY_MS: u32 = 10;
const IDLE_LOOP_DELAY_MS: u32 = 50;

pub struct ReaderApp {
    hardware: Hardware,
    display: Display,
    power: PowerManager,
    idle_ticks: u32,
}

impl ReaderApp {
    pub fn boot(mut hardware: Hardware, mut display: Display) -> Result<Self> {
        info!("Booting rr_reader");

        hardware.mount_storage()?;
        display.begin()?;
        display.show_boot_screen()?;
        display.flush_with_mode(RefreshMode::Full)?;

        info!("Boot complete");

        Ok(Self {
            hardware,
            display,
            power: PowerManager::new(),
            idle_ticks: 0,
        })
    }

    pub fn tick(&mut self) -> Result<()> {
        self.hardware.update_inputs();

        let has_activity = self.hardware.has_user_activity();
        if has_activity {
            self.idle_ticks = 0;
            self.power.mark_activity();
        } else {
            self.idle_ticks = self.idle_ticks.saturating_add(1);
            self.power.tick();
        }

        self.handle_input()?;

        // Flush display if dirty
        self.display.flush_if_dirty()?;

        // Auto-sleep
        if self.power.should_sleep() {
            info!("Auto-sleep after {} idle ticks", self.idle_ticks);
            self.power.enter_deep_sleep(None);
        }

        Ok(())
    }

    fn handle_input(&mut self) -> Result<()> {
        use Button::*;

        if self.hardware.input.logical_was_pressed(PageForward) {
            info!("Page forward");
        }
        if self.hardware.input.logical_was_pressed(PageBack) {
            info!("Page back");
        }

        // Power button long press → sleep
        if self.hardware.input.is_pressed(crate::input::BTN_POWER)
            && self.hardware.input.held_ms(crate::input::BTN_POWER) >= 2000
        {
            info!("Power long press → sleep");
            self.power.enter_deep_sleep(None);
        }

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
