// SD card driver stub. Full implementation requires esp-idf VFS FAT SDSPI bindings.
use anyhow::Result;
use log::info;

const SD_CS: i32 = 12;
const SD_SPI_FREQ_KHZ: i32 = 40_000;
const SD_MOUNT_POINT: &str = "/sdcard";

pub struct Storage {
    mounted: bool,
}

impl Storage {
    pub fn new() -> Self {
        Self { mounted: false }
    }

    pub fn mount(&mut self) -> Result<()> {
        info!(
            "SD card mount stub (CS=GPIO{}, {} MHz, {})",
            SD_CS,
            SD_SPI_FREQ_KHZ / 1000,
            SD_MOUNT_POINT
        );
        // TODO: implement using esp_vfs_fat_sdspi_mount via esp-idf-svc FFI
        // For now, the SD card hardware pins are documented and ready.
        // The shared SPI2 bus (GPIO8=SCLK, GPIO10=MOSI, GPIO7=MISO) is
        // initialized by the display driver. SD uses CS=GPIO12.
        self.mounted = true;
        Ok(())
    }

    pub fn is_mounted(&self) -> bool {
        self.mounted
    }

    pub fn vault_path(&self) -> &str {
        "/sdcard/vault"
    }

    pub fn ensure_vault_dirs(&self) -> Result<()> {
        info!("Vault dirs stub: {}", self.vault_path());
        Ok(())
    }
}
