pub mod esp_idf;
pub mod spi;

pub use self::spi::init_shared_spi_bus;
pub use esp_idf::{
    FRONT_ADC_CHANNEL, INPUT_ADC_ATTEN, INPUT_ADC_WIDTH, POWER_BUTTON_GPIO, SIDE_ADC_CHANNEL,
};

pub const BOARD: &str = "esp32-c3-devkitm-1";
pub const FLASH_SIZE: &str = "16MB";
pub const SERIAL_BAUD: u32 = 115_200;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceModel {
    X4,
}
