mod app;
mod display;
mod hardware;
mod power;

use anyhow::Result;
use app::ReaderApp;
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::reset::{ResetReason, WakeupReason};
use log::info;

fn main() -> Result<()> {
    esp_idf_hal::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("Starting rr_reader");
    info!(
        "Reset after {:?}; wakeup due to {:?}",
        ResetReason::get(),
        WakeupReason::get()
    );

    let peripherals = Peripherals::take()?;
    let hardware = hardware::Hardware::new()?;
    hardware.log_detected_model();

    power::handle_wakeup(WakeupReason::get())?;

    let display = display::Display::new(peripherals)?;
    let mut app = ReaderApp::boot(hardware, display)?;

    loop {
        app.tick()?;
        FreeRtos::delay_ms(app.loop_delay_ms());
    }
}
