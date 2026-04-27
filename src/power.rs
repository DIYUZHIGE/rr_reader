use anyhow::Result;
use esp_idf_hal::reset::WakeupReason;
use esp_idf_hal::sys;
use log::{debug, info};

const POWER_BUTTON_PIN: u64 = 3;
const DEFAULT_SLEEP_TIMEOUT_SECS: u32 = 5 * 60; // 5 minutes

pub struct PowerManager {
    idle_ticks: u32,
    sleep_timeout_ticks: u32, // in main loop ticks (~10ms each)
    power_saving: bool,
}

impl PowerManager {
    pub fn new() -> Self {
        Self {
            idle_ticks: 0,
            sleep_timeout_ticks: DEFAULT_SLEEP_TIMEOUT_SECS * 100, // 5 min
            power_saving: false,
        }
    }

    pub fn mark_activity(&mut self) {
        self.idle_ticks = 0;
        if self.power_saving {
            self.set_power_saving(false);
        }
    }

    pub fn tick(&mut self) {
        self.idle_ticks = self.idle_ticks.saturating_add(1);
    }

    pub fn should_sleep(&self) -> bool {
        self.idle_ticks >= self.sleep_timeout_ticks
    }

    pub fn set_sleep_timeout_secs(&mut self, secs: u32) {
        self.sleep_timeout_ticks = secs * 100;
    }

    pub fn set_power_saving(&mut self, enable: bool) {
        if enable && !self.power_saving {
            debug!("Entering power saving mode");
            self.power_saving = true;
        } else if !enable && self.power_saving {
            debug!("Exiting power saving mode");
            self.power_saving = false;
        }
    }

    pub fn enter_deep_sleep(&self, timer_secs: Option<u64>) {
        info!("Entering deep sleep...");

        if let Some(secs) = timer_secs {
            info!("Timer wakeup: {} seconds", secs);
            unsafe {
                sys::esp_sleep_enable_timer_wakeup(secs * 1_000_000);
            }
        }

        // Wake on power button GPIO3 LOW
        unsafe {
            sys::esp_deep_sleep_enable_gpio_wakeup(
                1u64 << POWER_BUTTON_PIN,
                sys::esp_deepsleep_gpio_wake_up_mode_t_ESP_GPIO_WAKEUP_GPIO_LOW,
            );
            sys::esp_deep_sleep_start();
        }
    }
}

/// Standalone wakeup handler. Returns true for normal boot, false if should go back to sleep.
pub fn handle_wakeup(reason: WakeupReason) -> Result<bool> {
    match reason {
        WakeupReason::Unknown => {
            debug!("Wakeup: cold boot or unknown");
            Ok(true)
        }
        WakeupReason::Button => {
            info!("Wakeup: button (power key)");
            Ok(true)
        }
        WakeupReason::Timer => {
            info!("Wakeup: timer");
            Ok(true)
        }
        _ => {
            debug!("Wakeup: {:?}", reason);
            Ok(true)
        }
    }
}
