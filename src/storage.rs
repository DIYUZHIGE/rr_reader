// SD card driver over shared SPI2 bus.
//
// The SPI bus is initialized by calling init_spi_bus() first (via
// spi_bus_initialize with GPIO pins matching the crosspoint hardware: SCLK=8,
// MISO=7, MOSI=10). After that, the display adds itself as a device on the same
// bus. Finally, mount() lets esp_vfs_fat_sdspi_mount handle the SD card device
// setup internally and mounts FAT via VFS.
//
// Once mounted, standard std::fs operations work under /sdcard.

use anyhow::{anyhow, Result};
use esp_idf_hal::sys;
use log::{info, warn};
use std::ffi::CString;
use std::fs;
use std::path::Path;

const SD_MOUNT_POINT: &str = "/sdcard";
const SPI2_HOST: sys::spi_host_device_t = sys::spi_host_device_t_SPI2_HOST;

pub struct Storage {
    mounted: bool,
    card: *mut sys::sdmmc_card_t,
}

/// Initialize the shared SPI2 bus. Must be called BEFORE Display::new() so
/// the display can add itself as a device on the already-initialized bus.
///
/// Uses the same GPIO pins as the crosspoint firmware: SCLK=8, MISO=7,
/// MOSI=10. The SD card (CS=GPIO12) and display (CS=GPIO21) share this bus.
pub fn init_spi_bus() -> Result<()> {
    info!("Initializing SPI2 bus (shared: SD card + display)");
    let bus_config = sys::spi_bus_config_t {
        __bindgen_anon_1: sys::spi_bus_config_t__bindgen_ty_1 { mosi_io_num: 10 },
        __bindgen_anon_2: sys::spi_bus_config_t__bindgen_ty_2 { miso_io_num: 7 },
        sclk_io_num: 8,
        __bindgen_anon_3: sys::spi_bus_config_t__bindgen_ty_3 {
            quadwp_io_num: -1,
        },
        __bindgen_anon_4: sys::spi_bus_config_t__bindgen_ty_4 {
            quadhd_io_num: -1,
        },
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

impl Storage {
    pub fn new() -> Self {
        Self {
            mounted: false,
            card: std::ptr::null_mut(),
        }
    }

    /// Mount the SD card. The SPI bus must already be initialized via
    /// init_spi_bus(). This adds the SD device, initializes the card,
    /// and registers the FAT filesystem at /sdcard.
    pub fn mount(&mut self) -> Result<()> {
        info!(
            "Mounting SD card (CS=GPIO12, {} MHz, {})",
            sys::SDMMC_FREQ_DEFAULT / 1000,
            SD_MOUNT_POINT
        );

        let host = unsafe { sys::rr_sdspi_host_default(SPI2_HOST) };

        // ── Mount FAT filesystem (handles SD device init + card init) ──
        let device_config = sys::sdspi_device_config_t {
            host_id: SPI2_HOST,
            gpio_cs: 12,
            gpio_cd: -1, // not connected
            gpio_wp: -1, // not connected
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

        let base_path = CString::new(SD_MOUNT_POINT).unwrap();
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

    pub fn is_mounted(&self) -> bool {
        self.mounted
    }

    /// Ensure the vault directory structure exists.
    pub fn ensure_vault_dirs(&self) -> Result<()> {
        let vault = Path::new(SD_MOUNT_POINT).join("vault");
        let notes = vault.join("notes");
        fs::create_dir_all(&notes).map_err(|e| anyhow!("mkdir {:?}: {}", notes, e))?;
        info!("Vault dirs ready: {:?}", vault);
        Ok(())
    }

    /// List .md files recursively under /sdcard/vault/notes.
    /// Returns paths relative to /sdcard/vault/notes.
    pub fn list_markdown_files(&self, base: &str) -> Result<Vec<String>> {
        let dir = Path::new(SD_MOUNT_POINT).join("vault").join("notes").join(base);
        let mut files = Vec::new();

        if !dir.exists() {
            return Ok(files);
        }

        for entry in fs::read_dir(&dir).map_err(|e| anyhow!("read_dir {:?}: {}", dir, e))? {
            let entry = entry.map_err(|e| anyhow!("dir entry: {}", e))?;
            let path = entry.path();
            if path.is_dir() {
                let sub = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let sub_base = if base.is_empty() {
                    sub
                } else {
                    format!("{}/{}", base, sub)
                };
                files.extend(self.list_markdown_files(&sub_base)?);
            } else if path.extension().map_or(false, |e| e == "md") {
                let rel = if base.is_empty() {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                } else {
                    format!(
                        "{}/{}",
                        base,
                        path.file_name().unwrap_or_default().to_string_lossy()
                    )
                };
                files.push(rel);
            }
        }
        Ok(files)
    }

    /// Read a markdown file relative to /sdcard/vault/notes.
    pub fn read_markdown_file(&self, rel_path: &str) -> Result<String> {
        let full = Path::new(SD_MOUNT_POINT)
            .join("vault")
            .join("notes")
            .join(rel_path);
        fs::read_to_string(&full).map_err(|e| anyhow!("read {:?}: {}", full, e))
    }

    /// Read any file by absolute path under /sdcard.
    pub fn read_file(&self, path: &str) -> Result<String> {
        fs::read_to_string(path).map_err(|e| anyhow!("read {}: {}", path, e))
    }

    pub fn vault_path(&self) -> String {
        format!("{}/vault/notes", SD_MOUNT_POINT)
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        if self.mounted {
            info!("Unmounting SD card");
            let base_path = CString::new(SD_MOUNT_POINT).unwrap();
            unsafe {
                if !self.card.is_null() {
                    sys::esp_vfs_fat_sdcard_unmount(base_path.as_ptr(), self.card);
                }
            }
        }
    }
}
