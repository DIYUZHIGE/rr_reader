mod spi;

pub use self::spi::init_shared_spi_bus;

use crate::input::InputManager;
use crate::network::{NetworkManager, WifiStatus};
use crate::storage::Storage;
use anyhow::Result;
use esp_idf_hal::modem::Modem;
use log::info;

pub const BOARD: &str = "esp32-c3-devkitm-1";
pub const FLASH_SIZE: &str = "16MB";
pub const SERIAL_BAUD: u32 = 115_200;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceModel {
    X4,
}

pub struct Hardware {
    pub model: DeviceModel,
    pub input: InputManager,
    pub storage: Storage,
    pub network: NetworkManager,
}

impl Hardware {
    pub fn new(input: InputManager, modem: Modem<'static>) -> Result<Self> {
        Ok(Self {
            model: DeviceModel::X4,
            input,
            storage: Storage::new(),
            network: NetworkManager::new(modem),
        })
    }

    pub fn log_detected_model(&self) {
        info!(
            "Hardware: {:?}; board={}; flash={}; serial={} baud",
            self.model, BOARD, FLASH_SIZE, SERIAL_BAUD
        );
    }

    /// Mount SD card storage. Must be called after display SPI init (they share SPI2).
    pub fn mount_storage(&mut self) -> Result<()> {
        self.storage.mount()?;
        self.storage.ensure_vault_dirs()?;
        Ok(())
    }

    pub fn connect_wifi_from_storage(&mut self) -> WifiStatus {
        self.network.connect_from_storage(&self.storage)
    }

    pub fn suspend_wifi_for_reader(&mut self) {
        self.network.suspend();
    }

    pub fn resume_wifi_after_reader(&mut self) {
        self.network.resume();
    }

    /// Update button state. Call once per loop iteration.
    pub fn update_inputs(&mut self) {
        self.input.update();
    }

    /// Returns true if any button was pressed or released this tick.
    pub fn has_user_activity(&self) -> bool {
        self.input.has_user_activity()
    }
}
