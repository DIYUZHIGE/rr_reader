use super::SD_MOUNT_POINT;
use anyhow::{anyhow, Result};
use esp_idf_hal::sys;
use log::{info, warn};
use std::ffi::CString;

const SPI2_HOST: sys::spi_host_device_t = sys::spi_host_device_t_SPI2_HOST;

pub struct Storage {
    mounted: bool,
    card: *mut sys::sdmmc_card_t,
}

impl Storage {
    pub fn new() -> Self {
        Self {
            mounted: false,
            card: std::ptr::null_mut(),
        }
    }

    /// Mount the SD card. The shared SPI2 bus must already be initialized.
    /// This adds the SD device, initializes the card, and registers FAT at
    /// /sdcard.
    pub fn mount(&mut self) -> Result<()> {
        info!(
            "Mounting SD card (CS=GPIO12, {} MHz, {})",
            sys::SDMMC_FREQ_DEFAULT / 1000,
            SD_MOUNT_POINT
        );

        let host = unsafe { sys::rr_sdspi_host_default(SPI2_HOST) };

        let device_config = sys::sdspi_device_config_t {
            host_id: SPI2_HOST,
            gpio_cs: 12,
            gpio_cd: -1,  // not connected
            gpio_wp: -1,  // not connected
            gpio_int: -1, // not connected
            gpio_wp_polarity: false,
        };

        let mut card: *mut sys::sdmmc_card_t = std::ptr::null_mut();
        let mount_config = sys::esp_vfs_fat_mount_config_t {
            format_if_mount_failed: false,
            max_files: 4,
            allocation_unit_size: 0,
            disk_status_check_enable: false,
            use_one_fat: false,
        };

        let base_path = CString::new(SD_MOUNT_POINT)?;
        let ret = unsafe {
            sys::esp_vfs_fat_sdspi_mount(
                base_path.as_ptr(),
                &host,
                &device_config,
                &mount_config,
                &mut card,
            )
        };

        if ret != 0 {
            warn!("FAT mount failed: {:?}", ret);
            return Err(anyhow!("SD card FAT mount failed (error {:?})", ret));
        }

        self.card = card;
        self.mounted = true;
        info!("SD card mounted at {}", SD_MOUNT_POINT);
        Ok(())
    }


}

impl Drop for Storage {
    fn drop(&mut self) {
        if self.mounted {
            info!("Unmounting SD card");
            if let Ok(base_path) = CString::new(SD_MOUNT_POINT) {
                unsafe {
                    if !self.card.is_null() {
                        sys::esp_vfs_fat_sdcard_unmount(base_path.as_ptr(), self.card);
                    }
                }
            }
        }
    }
}
