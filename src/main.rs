#![allow(dead_code)]

mod app;
mod browser;
mod display;
mod font;
mod hardware;
mod input;
mod network;
mod power;
mod reader;
mod storage;
mod text;
mod time;

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

    let Peripherals { pins, modem, .. } = Peripherals::take()?;

    // Input manager uses raw sys:: calls (doesn't own peripherals)
    let input_manager = input::InputManager::new()?;

    // Initialize the shared SPI2 bus via SDSPI host.
    // This must happen BEFORE Display::new() so the display can add itself
    // as a device on the already-initialized bus.
    hardware::init_shared_spi_bus()?;

    // Display adds itself as a raw SPI device on the shared bus.
    // Takes full Peripherals (uses GPIO pins; SPI peripherals are unused here).
    let display = display::Display::new(pins)?;

    // Initialize hardware (combines input and storage)
    let mut hardware = hardware::Hardware::new(input_manager, modem)?;
    hardware.log_detected_model();

    // Handle wakeup
    power::handle_wakeup(WakeupReason::get(), &mut hardware.input)?;

    // Boot the app (mounts SD, initializes display, loads fonts, shows boot screen)
    let mut app = ReaderApp::boot(hardware, display)?;

    // Connect WiFi after boot to avoid stack frame overlap with display/font init
    app.connect_wifi();

    info!("Entering main loop");
    loop {
        app.tick()?;
        FreeRtos::delay_ms(app.loop_delay_ms());
    }
}
