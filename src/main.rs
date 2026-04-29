#![allow(dead_code)]

mod app;
mod display;
mod font;
mod hardware;
mod input;
mod power;
mod storage;

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
        "Reset: {:?}; Wakeup: {:?}",
        ResetReason::get(),
        WakeupReason::get()
    );

    let peripherals = Peripherals::take()?;

    // Input manager uses raw sys:: calls (doesn't own peripherals)
    let input_manager = input::InputManager::new()?;

    // Display takes the full Peripherals (owns SPI2 + GPIO pins)
    let display = display::Display::new(peripherals)?;

    // Initialize hardware (combines input and storage)
    let hardware = hardware::Hardware::new(input_manager)?;
    hardware.log_detected_model();

    // Handle wakeup
    power::handle_wakeup(WakeupReason::get())?;

    // Boot the app (mounts SD, initializes display, shows boot screen)
    let mut app = ReaderApp::boot(hardware, display)?;

    info!("Entering main loop");
    loop {
        app.tick()?;
        FreeRtos::delay_ms(app.loop_delay_ms());
    }
}
