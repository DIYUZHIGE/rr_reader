use anyhow::Result;
use esp_idf_hal::sys;
use log::info;

const SPI2_HOST: sys::spi_host_device_t = sys::spi_host_device_t_SPI2_HOST;

/// Initialize the shared SPI2 bus. Must be called before Display::new() so the
/// display can add itself as a device on the already-initialized bus.
///
/// Uses the same GPIO pins as the crosspoint firmware: SCLK=8, MISO=7,
/// MOSI=10. The SD card (CS=GPIO12) and display (CS=GPIO21) share this bus.
pub fn init_shared_spi_bus() -> Result<()> {
    info!("Initializing SPI2 bus (shared: SD card + display)");
    let bus_config = sys::spi_bus_config_t {
        __bindgen_anon_1: sys::spi_bus_config_t__bindgen_ty_1 { mosi_io_num: 10 },
        __bindgen_anon_2: sys::spi_bus_config_t__bindgen_ty_2 { miso_io_num: 7 },
        sclk_io_num: 8,
        __bindgen_anon_3: sys::spi_bus_config_t__bindgen_ty_3 { quadwp_io_num: -1 },
        __bindgen_anon_4: sys::spi_bus_config_t__bindgen_ty_4 { quadhd_io_num: -1 },
        max_transfer_sz: 16384,
        ..Default::default()
    };
    unsafe {
        sys::esp!(sys::spi_bus_initialize(
            SPI2_HOST,
            &bus_config,
            sys::spi_common_dma_t_SPI_DMA_CH_AUTO,
        ))?;
    }
    info!("SPI2 bus initialized");
    Ok(())
}
