use super::path::{
    asset_candidate_relative_paths, clean_asset_path, file_name_matches, validated_relative_path,
    validated_sdcard_absolute_path,
};
use super::{Storage, SD_MOUNT_POINT, VAULT_DIR};
use anyhow::{anyhow, Result};
use log::info;
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const READ_CHUNK_SIZE: usize = 1024;

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

    /// Read a markdown file relative to /sdcard/vault.
    pub fn read_markdown_file(&self, rel_path: &str) -> Result<String> {
        let rel = validated_relative_path(rel_path)?;
        let full = Path::new(SD_MOUNT_POINT).join(VAULT_DIR).join(rel);
        fs::read_to_string(&full).map_err(|e| anyhow!("read {:?}: {}", full, e))
    }

    /// Read a binary vault asset referenced from a markdown file.
    ///
    /// Relative asset paths are resolved from the markdown file's folder.
    /// Leading slash paths are treated as vault-root relative paths.
    pub fn read_asset_relative_to(
        &self,
        markdown_rel_path: &str,
        asset_path: &str,
    ) -> Result<Vec<u8>> {
        let candidates = asset_candidate_relative_paths(markdown_rel_path, asset_path)?;
        let vault_root = Path::new(SD_MOUNT_POINT).join(VAULT_DIR);

        for rel in &candidates {
            let full = vault_root.join(rel);
            if let Ok(bytes) = fs::read(&full) {
                return Ok(bytes);
            }
        }

        let asset_path = clean_asset_path(asset_path);
        if let Some(file_name) = Path::new(&asset_path).file_name() {
            if let Some(full) = self.find_vault_file_by_name(&vault_root, file_name)? {
                return fs::read(&full).map_err(|e| anyhow!("read {:?}: {}", full, e));
            }
        }

        Err(anyhow!(
            "image not found: {} (tried {:?})",
            asset_path,
            candidates
        ))
    }

    pub fn open_asset_relative_to(
        &self,
        markdown_rel_path: &str,
        asset_path: &str,
    ) -> Result<BufReader<File>> {
        let candidates = asset_candidate_relative_paths(markdown_rel_path, asset_path)?;
        let vault_root = Path::new(SD_MOUNT_POINT).join(VAULT_DIR);

        for rel in &candidates {
            let full = vault_root.join(rel);
            if let Ok(file) = File::open(&full) {
                return Ok(BufReader::with_capacity(2048, file));
            }
        }

        let asset_path = clean_asset_path(asset_path);
        if let Some(file_name) = Path::new(&asset_path).file_name() {
            if let Some(full) = self.find_vault_file_by_name(&vault_root, file_name)? {
                let file = File::open(&full).map_err(|e| anyhow!("open {:?}: {}", full, e))?;
                return Ok(BufReader::with_capacity(2048, file));
            }
        }

        Err(anyhow!(
            "image not found: {} (tried {:?})",
            asset_path,
            candidates
        ))
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
        let path = validated_sdcard_absolute_path(path)?;
        fs::read_to_string(path).map_err(|e| anyhow!("read {:?}: {}", path, e))
    }

    pub fn vault_path(&self) -> String {
        format!("{}/{}", SD_MOUNT_POINT, VAULT_DIR)
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

    fn markdown_full_path(&self, rel_path: &str) -> Result<PathBuf> {
        let rel = validated_relative_path(rel_path)?;
        Ok(Path::new(SD_MOUNT_POINT).join(VAULT_DIR).join(rel))
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
