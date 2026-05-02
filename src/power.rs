use crate::input::{InputManager, BTN_POWER};
use crate::time::now_ms;
use anyhow::Result;
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::reset::WakeupReason;
use esp_idf_hal::sys;
use log::{debug, info};

const POWER_BUTTON_PIN: u64 = 3;
const DEFAULT_SLEEP_TIMEOUT_SECS: u32 = 5 * 60; // 5 minutes
pub const POWER_BUTTON_SLEEP_MS: u32 = 2000;

pub struct PowerManager {
    last_activity_ms: u64,
    sleep_timeout_ms: u64,
    power_saving: bool,
}

impl PowerManager {
    pub fn new() -> Self {
        Self {
            last_activity_ms: now_ms(),
            sleep_timeout_ms: DEFAULT_SLEEP_TIMEOUT_SECS as u64 * 1000,
            power_saving: false,
        }
    }

    pub fn mark_activity(&mut self) {
        self.last_activity_ms = now_ms();
        if self.power_saving {
            self.set_power_saving(false);
        }
    }

    pub fn tick(&mut self) {}

    pub fn should_sleep(&self) -> bool {
        now_ms().saturating_sub(self.last_activity_ms) >= self.sleep_timeout_ms
    }

    pub fn set_sleep_timeout_secs(&mut self, secs: u32) {
        self.sleep_timeout_ms = secs as u64 * 1000;
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
        enter_deep_sleep_now(timer_secs);
    }
}

/// Standalone wakeup handler. Returns true for normal boot, false if should go back to sleep.
pub fn handle_wakeup(reason: WakeupReason, input: &mut InputManager) -> Result<bool> {
    match reason {
        WakeupReason::Unknown => {
            debug!("Wakeup: cold boot or unknown");
            Ok(true)
        }
        WakeupReason::Button => {
            info!("Wakeup: button (power key)");
            verify_power_button_wakeup(input);
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

fn verify_power_button_wakeup(input: &mut InputManager) {
    let start_ms = now_ms();
    let hold_threshold_ms = POWER_BUTTON_SLEEP_MS;

    input.update();
    while !input.is_pressed(BTN_POWER) && now_ms().saturating_sub(start_ms) < 1000 {
        FreeRtos::delay_ms(10);
        input.update();
    }

    if !input.is_pressed(BTN_POWER) {
        enter_deep_sleep_now(None);
    }

    while input.is_pressed(BTN_POWER) && input.held_ms(BTN_POWER) < hold_threshold_ms {
        FreeRtos::delay_ms(10);
        input.update();
    }

    if input.held_ms(BTN_POWER) < hold_threshold_ms {
        enter_deep_sleep_now(None);
    }
}

fn enter_deep_sleep_now(timer_secs: Option<u64>) {
    info!("Entering deep sleep...");

    if let Some(secs) = timer_secs {
        info!("Timer wakeup: {} seconds", secs);
        unsafe {
            sys::esp_sleep_enable_timer_wakeup(secs * 1_000_000);
        }
    }

    wait_for_power_button_release();

    // Wake on power button GPIO3 LOW. Arm only after release so holding the
    // sleep button cannot immediately wake the device again.
    unsafe {
        sys::esp_deep_sleep_enable_gpio_wakeup(
            1u64 << POWER_BUTTON_PIN,
            sys::esp_deepsleep_gpio_wake_up_mode_t_ESP_GPIO_WAKEUP_GPIO_LOW,
        );
        sys::esp_deep_sleep_start();
    }
}

fn wait_for_power_button_release() {
    while unsafe { sys::gpio_get_level(sys::gpio_num_t_GPIO_NUM_3) } == 0 {
        FreeRtos::delay_ms(50);
    }
}
