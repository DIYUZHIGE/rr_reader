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
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path};

const SD_MOUNT_POINT: &str = "/sdcard";
const VAULT_DIR: &str = "vault";
const READ_CHUNK_SIZE: usize = 1024;
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

    pub fn is_mounted(&self) -> bool {
        self.mounted
    }

    /// Ensure the vault directory structure exists.
    pub fn ensure_vault_dirs(&self) -> Result<()> {
        let vault = Path::new(SD_MOUNT_POINT).join(VAULT_DIR);
        let notes = vault.join("notes");
        fs::create_dir_all(&notes).map_err(|e| anyhow!("mkdir {:?}: {}", notes, e))?;
        info!("Vault dirs ready: {:?}", vault);
        Ok(())
    }

    /// List markdown files recursively under /sdcard/vault.
    ///
    /// This covers both a copied Obsidian vault directly under /sdcard/vault
    /// and the eventual sync cache layout under /sdcard/vault/notes.
    pub fn list_markdown_files(&self, base: &str) -> Result<Vec<String>> {
        let base = Self::validated_relative_path(base)?;
        let scan_root = Path::new(SD_MOUNT_POINT).join(VAULT_DIR).join(base);
        let mut files = Vec::new();
        self.collect_markdown_files(&scan_root, &scan_root, &mut files)?;

        if files.is_empty() {
            info!("No markdown files found under {:?}", scan_root);
        } else {
            info!("Found {} markdown files under {:?}", files.len(), scan_root);
        }

        Ok(files)
    }

    fn collect_markdown_files(
        &self,
        root: &Path,
        dir: &Path,
        files: &mut Vec<String>,
    ) -> Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(dir).map_err(|e| anyhow!("read_dir {:?}: {}", dir, e))? {
            let entry = entry.map_err(|e| anyhow!("dir entry: {}", e))?;
            let path = entry.path();
            if path.is_dir() {
                self.collect_markdown_files(root, &path, files)?;
            } else if Self::is_markdown_file(&path) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push(rel);
            }
        }

        Ok(())
    }

    fn is_markdown_file(path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
            .unwrap_or(false)
    }

    /// Read a markdown file relative to /sdcard/vault.
    pub fn read_markdown_file(&self, rel_path: &str) -> Result<String> {
        let rel = Self::validated_relative_path(rel_path)?;
        let full = Path::new(SD_MOUNT_POINT).join(VAULT_DIR).join(rel);
        fs::read_to_string(&full).map_err(|e| anyhow!("read {:?}: {}", full, e))
    }

    pub fn markdown_file_len(&self, rel_path: &str) -> Result<usize> {
        let full = self.markdown_full_path(rel_path)?;
        let len = fs::metadata(&full)
            .map_err(|e| anyhow!("metadata {:?}: {}", full, e))?
            .len();
        usize::try_from(len).map_err(|_| anyhow!("file too large for platform: {:?}", full))
    }

    pub fn read_markdown_range(&self, rel_path: &str, start: usize, end: usize) -> Result<String> {
        if start > end {
            return Err(anyhow!("invalid read range: {}..{}", start, end));
        }

        let full = self.markdown_full_path(rel_path)?;
        let mut file = File::open(&full).map_err(|e| anyhow!("open {:?}: {}", full, e))?;
        let len = file
            .metadata()
            .map_err(|e| anyhow!("metadata {:?}: {}", full, e))?
            .len() as usize;
        if end > len {
            return Err(anyhow!(
                "read range exceeds file length: {}..{} > {}",
                start,
                end,
                len
            ));
        }

        let mut buf = vec![0u8; end - start];
        file.seek(SeekFrom::Start(start as u64))
            .map_err(|e| anyhow!("seek {:?}: {}", full, e))?;
        file.read_exact(&mut buf)
            .map_err(|e| anyhow!("read {:?}: {}", full, e))?;
        String::from_utf8(buf).map_err(|e| anyhow!("read {:?}: invalid UTF-8: {}", full, e))
    }

    pub fn scan_markdown_chars<F>(&self, rel_path: &str, mut visit: F) -> Result<()>
    where
        F: FnMut(usize, char) -> Result<()>,
    {
        let full = self.markdown_full_path(rel_path)?;
        let mut file = File::open(&full).map_err(|e| anyhow!("open {:?}: {}", full, e))?;
        let mut chunk = [0u8; READ_CHUNK_SIZE];
        let mut pending = Vec::with_capacity(READ_CHUNK_SIZE + 4);
        let mut pending_offset = 0usize;

        loop {
            let read = file
                .read(&mut chunk)
                .map_err(|e| anyhow!("read {:?}: {}", full, e))?;
            if read == 0 {
                break;
            }

            pending.extend_from_slice(&chunk[..read]);
            let valid_len = match std::str::from_utf8(&pending) {
                Ok(valid) => valid.len(),
                Err(e) if e.error_len().is_none() => e.valid_up_to(),
                Err(e) => return Err(anyhow!("read {:?}: invalid UTF-8: {}", full, e)),
            };

            let valid = std::str::from_utf8(&pending[..valid_len])
                .map_err(|e| anyhow!("read {:?}: invalid UTF-8: {}", full, e))?;
            for (offset, ch) in valid.char_indices() {
                visit(pending_offset + offset, ch)?;
            }

            pending.drain(..valid_len);
            pending_offset += valid_len;
        }

        if !pending.is_empty() {
            let valid = std::str::from_utf8(&pending)
                .map_err(|e| anyhow!("read {:?}: invalid UTF-8: {}", full, e))?;
            for (offset, ch) in valid.char_indices() {
                visit(pending_offset + offset, ch)?;
            }
        }

        Ok(())
    }

    /// Read any file by absolute path under /sdcard.
    pub fn read_file(&self, path: &str) -> Result<String> {
        let path = Self::validated_sdcard_absolute_path(path)?;
        fs::read_to_string(path).map_err(|e| anyhow!("read {:?}: {}", path, e))
    }

    pub fn vault_path(&self) -> String {
        format!("{}/{}", SD_MOUNT_POINT, VAULT_DIR)
    }

    fn markdown_full_path(&self, rel_path: &str) -> Result<std::path::PathBuf> {
        let rel = Self::validated_relative_path(rel_path)?;
        Ok(Path::new(SD_MOUNT_POINT).join(VAULT_DIR).join(rel))
    }

    fn validated_relative_path(path: &str) -> Result<&Path> {
        let path = Path::new(path);
        if path.is_absolute() {
            return Err(anyhow!("absolute path is not allowed: {:?}", path));
        }

        for component in path.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(anyhow!("path escapes vault: {:?}", path));
                }
            }
        }

        Ok(path)
    }

    fn validated_sdcard_absolute_path(path: &str) -> Result<&Path> {
        let path = Path::new(path);
        if !path.is_absolute() || !path.starts_with(SD_MOUNT_POINT) {
            return Err(anyhow!("path is outside {}: {:?}", SD_MOUNT_POINT, path));
        }

        for component in path.components() {
            if matches!(component, Component::ParentDir | Component::Prefix(_)) {
                return Err(anyhow!("path escapes {}: {:?}", SD_MOUNT_POINT, path));
            }
        }

        Ok(path)
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
