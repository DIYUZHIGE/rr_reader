use anyhow::{anyhow, Result};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use super::{SyncManifestEntry, SYNC_CONTENT_ROOT, SYNC_STATUS_PATH};
use crate::storage::RemotelySaveConfig;
use crate::time;

const SYNC_STATUS_TMP_PATH: &str = "/sdcard/vault/.rr_sync_status.tmp";
const SYNC_STATUS_BAK_PATH: &str = "/sdcard/vault/.rr_sync_status.bak";
const SYNC_STATUS_ENTRIES_TMP_PATH: &str = "/sdcard/vault/.rr_sync_status.entries.tmp";
const COPY_BUFFER_BYTES: usize = 1024;

pub(super) struct SyncManifestWriter {
    entries_file: File,
}

impl SyncManifestWriter {
    pub(super) fn new() -> Result<Self> {
        let _ = fs::remove_file(SYNC_STATUS_ENTRIES_TMP_PATH);
        let entries_file = File::create(SYNC_STATUS_ENTRIES_TMP_PATH)
            .map_err(|e| anyhow!("create {}: {}", SYNC_STATUS_ENTRIES_TMP_PATH, e))?;
        Ok(Self { entries_file })
    }

    pub(super) fn append_entry(&mut self, key: &str, entry: &SyncManifestEntry) -> Result<()> {
        write_manifest_entry(&mut self.entries_file, key, entry)
    }

    pub(super) fn entries_path(&self) -> &'static str {
        SYNC_STATUS_ENTRIES_TMP_PATH
    }

    pub(super) fn finalize(
        mut self,
        config: &RemotelySaveConfig,
        downloaded: usize,
        skipped: usize,
        deleted: usize,
    ) -> Result<()> {
        self.entries_file
            .flush()
            .map_err(|e| anyhow!("flush {}: {}", SYNC_STATUS_ENTRIES_TMP_PATH, e))?;
        drop(self.entries_file);

        write_status_file_from_entries(config, downloaded, skipped, deleted)?;
        let _ = fs::remove_file(SYNC_STATUS_ENTRIES_TMP_PATH);
        Ok(())
    }
}

pub(super) fn find_sync_manifest_entry(key: &str) -> Option<SyncManifestEntry> {
    let file = File::open(SYNC_STATUS_PATH).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let Ok(line) = line else {
            continue;
        };
        let Some((entry_key, entry)) = parse_manifest_line(&line) else {
            continue;
        };
        if entry_key == key {
            return Some(entry);
        }
    }

    None
}

pub(super) fn delete_stale_manifest_files(new_entries_path: &str) -> Result<usize> {
    let Ok(file) = File::open(SYNC_STATUS_PATH) else {
        return Ok(0);
    };

    let mut deleted = 0usize;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                log::warn!("could not read sync status line: {}", e);
                continue;
            }
        };
        let Some((key, entry)) = parse_manifest_line(&line) else {
            continue;
        };

        if manifest_entries_file_contains_key(new_entries_path, &key)? {
            continue;
        }
        if entry.local_path.is_empty() || !is_path_under_sync_content_root(&entry.local_path) {
            continue;
        }
        let path = Path::new(&entry.local_path);
        if path.exists() {
            let Ok(meta) = fs::metadata(path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            fs::remove_file(path).map_err(|e| anyhow!("remove stale {:?}: {}", path, e))?;
            deleted += 1;
            remove_empty_parent_dirs(path.parent());
        }
    }
    Ok(deleted)
}

fn manifest_entries_file_contains_key(entries_path: &str, key: &str) -> Result<bool> {
    let Ok(file) = File::open(entries_path) else {
        return Ok(false);
    };
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.map_err(|e| anyhow!("read {}: {}", entries_path, e))?;
        let Some((entry_key, _)) = parse_manifest_line(&line) else {
            continue;
        };
        if entry_key == key {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_path_under_sync_content_root(path: &str) -> bool {
    path.starts_with(SYNC_CONTENT_ROOT)
        && path.as_bytes().get(SYNC_CONTENT_ROOT.len()) == Some(&b'/')
}

fn remove_empty_parent_dirs(mut dir: Option<&Path>) {
    while let Some(path) = dir {
        if path == Path::new(SYNC_CONTENT_ROOT) {
            break;
        }
        match fs::remove_dir(path) {
            Ok(()) => dir = path.parent(),
            Err(_) => break,
        }
    }
}

fn parse_manifest_line(line: &str) -> Option<(String, SyncManifestEntry)> {
    let rest = line.strip_prefix("M\t")?;
    let mut parts = rest.split('\t');
    let key_raw = parts.next()?;
    let size_raw = parts.next()?;
    let etag_raw = parts.next()?;
    let local_path_raw = parts.next()?;
    let size = size_raw.parse::<u64>().ok()?;

    Some((
        unescape_manifest_field(key_raw),
        SyncManifestEntry {
            size,
            etag: unescape_manifest_field(etag_raw),
            local_path: unescape_manifest_field(local_path_raw),
        },
    ))
}

fn write_manifest_entry<W: Write>(
    writer: &mut W,
    key: &str,
    entry: &SyncManifestEntry,
) -> Result<()> {
    writer
        .write_all(b"M\t")
        .map_err(|e| anyhow!("write manifest entry: {}", e))?;
    write_escaped_manifest_field(writer, key)?;
    writer
        .write_all(b"\t")
        .map_err(|e| anyhow!("write manifest entry: {}", e))?;
    write!(writer, "{}\t", entry.size).map_err(|e| anyhow!("write manifest entry: {}", e))?;
    write_escaped_manifest_field(writer, &entry.etag)?;
    writer
        .write_all(b"\t")
        .map_err(|e| anyhow!("write manifest entry: {}", e))?;
    write_escaped_manifest_field(writer, &entry.local_path)?;
    writer
        .write_all(b"\n")
        .map_err(|e| anyhow!("write manifest entry: {}", e))?;
    Ok(())
}

fn write_escaped_manifest_field<W: Write>(writer: &mut W, value: &str) -> Result<()> {
    for ch in value.chars() {
        match ch {
            '\\' => writer.write_all(b"\\\\"),
            '\t' => writer.write_all(b"\\t"),
            '\n' => writer.write_all(b"\\n"),
            '\r' => writer.write_all(b"\\r"),
            _ => {
                let mut buf = [0u8; 4];
                writer.write_all(ch.encode_utf8(&mut buf).as_bytes())
            }
        }
        .map_err(|e| anyhow!("write escaped manifest field: {}", e))?;
    }
    Ok(())
}

fn unescape_manifest_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn write_status_file_from_entries(
    config: &RemotelySaveConfig,
    downloaded: usize,
    skipped: usize,
    deleted: usize,
) -> Result<()> {
    let now_ms = time::now_ms();
    let mut status = File::create(SYNC_STATUS_TMP_PATH)
        .map_err(|e| anyhow!("create {}: {}", SYNC_STATUS_TMP_PATH, e))?;

    write!(
        status,
        concat!(
            "sync_status=ok\n",
            "timestamp_ms={}\n",
            "endpoint={}\n",
            "region={}\n",
            "bucket={}\n",
            "prefix={}\n",
            "force_path_style={}\n",
            "downloaded={}\n",
            "skipped={}\n",
            "deleted={}\n",
            "manifest_version=1\n",
            "manifest_begin\n"
        ),
        now_ms,
        config.endpoint,
        config.region,
        config.bucket_name,
        config.remote_prefix,
        config.force_path_style,
        downloaded,
        skipped,
        deleted
    )
    .map_err(|e| anyhow!("write {} header: {}", SYNC_STATUS_TMP_PATH, e))?;

    let mut entries = File::open(SYNC_STATUS_ENTRIES_TMP_PATH)
        .map_err(|e| anyhow!("open {}: {}", SYNC_STATUS_ENTRIES_TMP_PATH, e))?;
    let mut buf = [0u8; COPY_BUFFER_BYTES];
    loop {
        let n = entries
            .read(&mut buf)
            .map_err(|e| anyhow!("read {}: {}", SYNC_STATUS_ENTRIES_TMP_PATH, e))?;
        if n == 0 {
            break;
        }
        status
            .write_all(&buf[..n])
            .map_err(|e| anyhow!("copy manifest entries: {}", e))?;
    }

    status
        .write_all(b"manifest_end\n")
        .map_err(|e| anyhow!("write {} footer: {}", SYNC_STATUS_TMP_PATH, e))?;
    status
        .flush()
        .map_err(|e| anyhow!("flush {}: {}", SYNC_STATUS_TMP_PATH, e))?;
    drop(status);

    let _ = fs::remove_file(SYNC_STATUS_BAK_PATH);
    if Path::new(SYNC_STATUS_PATH).exists() {
        if let Err(e) = fs::rename(SYNC_STATUS_PATH, SYNC_STATUS_BAK_PATH) {
            warn_status_backup_failure(e);
        }
    }

    fs::rename(SYNC_STATUS_TMP_PATH, SYNC_STATUS_PATH).map_err(|e| {
        let _ = fs::rename(SYNC_STATUS_BAK_PATH, SYNC_STATUS_PATH);
        anyhow!(
            "rename {} -> {}: {}",
            SYNC_STATUS_TMP_PATH,
            SYNC_STATUS_PATH,
            e
        )
    })?;
    let _ = fs::remove_file(SYNC_STATUS_BAK_PATH);
    Ok(())
}

fn warn_status_backup_failure(e: std::io::Error) {
    log::warn!("could not rotate sync status backup: {}", e);
}
