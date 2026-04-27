use anyhow::Result;
use log::info;

pub const BOARD: &str = "esp32-c3-devkitm-1";
pub const FLASH_SIZE: &str = "16MB";
pub const SERIAL_BAUD: u32 = 115_200;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceModel {
    X4,
}

pub struct Hardware {
    model: DeviceModel,
}

impl Hardware {
    pub fn new() -> Result<Self> {
        Ok(Self {
            model: DeviceModel::X4,
        })
    }

    pub fn log_detected_model(&self) {
        info!(
            "Hardware detect: {:?}; board={}; flash={}; serial={} baud",
            self.model, BOARD, FLASH_SIZE, SERIAL_BAUD
        );
    }

    pub fn mount_storage(&mut self) -> Result<()> {
        info!("Storage init placeholder");
        Ok(())
    }

    pub fn update_inputs(&mut self) -> Result<()> {
        Ok(())
    }

    pub fn has_user_activity(&self) -> bool {
        false
    }
}
