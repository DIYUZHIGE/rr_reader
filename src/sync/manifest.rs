use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use super::{SyncManifestEntry, SYNC_STATUS_PATH, VAULT_ROOT};
use crate::storage::RemotelySaveConfig;
use crate::time;

pub(super) fn read_sync_manifest() -> HashMap<String, SyncManifestEntry> {
    let mut out = HashMap::new();
    let Ok(contents) = fs::read_to_string(SYNC_STATUS_PATH) else {
        return out;
    };

    for line in contents.lines() {
        let Some(rest) = line.strip_prefix("M\t") else {
            continue;
        };
        let mut parts = rest.split('\t');
        let Some(key_raw) = parts.next() else {
            continue;
        };
        let Some(size_raw) = parts.next() else {
            continue;
        };
        let Some(etag_raw) = parts.next() else {
            continue;
        };
        let Some(local_path_raw) = parts.next() else {
            continue;
        };
        let Ok(size) = size_raw.parse::<u64>() else {
            continue;
        };

        out.insert(
            unescape_manifest_field(key_raw),
            SyncManifestEntry {
                size,
                etag: unescape_manifest_field(etag_raw),
                local_path: unescape_manifest_field(local_path_raw),
            },
        );
    }

    out
}

pub(super) fn delete_stale_manifest_files(
    previous_manifest: &HashMap<String, SyncManifestEntry>,
    seen_keys: &HashSet<String>,
) -> Result<usize> {
    let mut deleted = 0usize;
    for (key, entry) in previous_manifest {
        if seen_keys.contains(key) {
            continue;
        }
        if entry.local_path.is_empty() || !entry.local_path.starts_with(VAULT_ROOT) {
            continue;
        }
        let path = Path::new(&entry.local_path);
        if path == Path::new(VAULT_ROOT) || path == Path::new(SYNC_STATUS_PATH) {
            continue;
        }
        if path.exists() {
            fs::remove_file(path).map_err(|e| anyhow!("remove stale {:?}: {}", path, e))?;
            deleted += 1;
            remove_empty_parent_dirs(path.parent());
        }
    }
    Ok(deleted)
}

fn remove_empty_parent_dirs(mut dir: Option<&Path>) {
    while let Some(path) = dir {
        if path == Path::new(VAULT_ROOT) {
            break;
        }
        match fs::remove_dir(path) {
            Ok(()) => dir = path.parent(),
            Err(_) => break,
        }
    }
}

fn escape_manifest_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
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

pub(super) fn write_status_file(
    config: &RemotelySaveConfig,
    downloaded: usize,
    skipped: usize,
    deleted: usize,
    manifest: &HashMap<String, SyncManifestEntry>,
) -> Result<()> {
    let now_ms = time::now_ms();
    let mut status = format!(
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
    );

    let mut keys: Vec<&String> = manifest.keys().collect();
    keys.sort();
    for key in keys {
        if let Some(entry) = manifest.get(key) {
            status.push_str("M\t");
            status.push_str(&escape_manifest_field(key));
            status.push('\t');
            status.push_str(&entry.size.to_string());
            status.push('\t');
            status.push_str(&escape_manifest_field(&entry.etag));
            status.push('\t');
            status.push_str(&escape_manifest_field(&entry.local_path));
            status.push('\n');
        }
    }
    status.push_str("manifest_end\n");

    fs::write(SYNC_STATUS_PATH, status)
        .map_err(|e| anyhow!("write {}: {}", SYNC_STATUS_PATH, e))?;
    Ok(())
}
