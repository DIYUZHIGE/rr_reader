use super::path::{
    asset_candidate_relative_paths, clean_asset_path, file_name_matches, validated_relative_path,
};
use super::{Storage, SD_MOUNT_POINT, VAULT_DIR};
use anyhow::{anyhow, Result};
use log::info;
use std::fs;
use std::path::{Path, PathBuf};

const READER_CONFIG_FILE: &str = ".rr_reader.conf";
const BROWSER_ROOT_KEY: &str = "browser_root";

impl Storage {
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
        let base = validated_relative_path(base)?;
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

    pub fn read_browser_root_dir(&self) -> Result<String> {
        let config_path = Path::new(SD_MOUNT_POINT)
            .join(VAULT_DIR)
            .join(READER_CONFIG_FILE);
        if !config_path.exists() {
            return Ok(String::new());
        }

        let contents = fs::read_to_string(&config_path)
            .map_err(|e| anyhow!("read {:?}: {}", config_path, e))?;

        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == BROWSER_ROOT_KEY {
                    let value = value.trim();
                    validated_relative_path(value)?;
                    return Ok(value.to_string());
                }
            }
        }

        Ok(String::new())
    }

    pub fn write_browser_root_dir(&self, root: &str) -> Result<()> {
        validated_relative_path(root)?;

        let config_path = Path::new(SD_MOUNT_POINT)
            .join(VAULT_DIR)
            .join(READER_CONFIG_FILE);
        let contents = format!("{}={}\n", BROWSER_ROOT_KEY, root);
        fs::write(&config_path, contents).map_err(|e| anyhow!("write {:?}: {}", config_path, e))
    }

    /// Read a markdown file relative to /sdcard/vault.
    pub fn read_markdown_file(&self, rel_path: &str) -> Result<String> {
        let rel = validated_relative_path(rel_path)?;
        let full = Path::new(SD_MOUNT_POINT).join(VAULT_DIR).join(rel);
        fs::read_to_string(&full).map_err(|e| anyhow!("read {:?}: {}", full, e))
    }

    pub fn resolve_asset_path_relative_to(
        &self,
        markdown_rel_path: &str,
        asset_path: &str,
    ) -> Result<String> {
        let full = self.resolve_asset_full_path(markdown_rel_path, asset_path)?;
        Ok(full.to_string_lossy().to_string())
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

    fn resolve_asset_full_path(
        &self,
        markdown_rel_path: &str,
        asset_path: &str,
    ) -> Result<PathBuf> {
        let candidates = asset_candidate_relative_paths(markdown_rel_path, asset_path)?;
        let vault_root = Path::new(SD_MOUNT_POINT).join(VAULT_DIR);

        for rel in &candidates {
            let full = vault_root.join(rel);
            if full.is_file() {
                return Ok(full);
            }
        }

        let cleaned_asset_path = clean_asset_path(asset_path);
        if let Some(file_name) = Path::new(&cleaned_asset_path).file_name() {
            if let Some(full) = self.find_vault_file_by_name(&vault_root, file_name)? {
                if full.is_file() {
                    return Ok(full);
                }
            }
        }

        Err(anyhow!(
            "image not found: {} (tried {:?})",
            cleaned_asset_path,
            candidates
        ))
    }

    fn find_vault_file_by_name(
        &self,
        dir: &Path,
        file_name: &std::ffi::OsStr,
    ) -> Result<Option<PathBuf>> {
        if !dir.exists() {
            return Ok(None);
        }

        for entry in fs::read_dir(dir).map_err(|e| anyhow!("read_dir {:?}: {}", dir, e))? {
            let entry = entry.map_err(|e| anyhow!("dir entry: {}", e))?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = self.find_vault_file_by_name(&path, file_name)? {
                    return Ok(Some(found));
                }
            } else if file_name_matches(path.file_name(), file_name) {
                return Ok(Some(path));
            }
        }

        Ok(None)
    }
}
