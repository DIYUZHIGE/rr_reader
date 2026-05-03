use anyhow::{anyhow, Result};
use std::path::{Component, Path, PathBuf};

use super::SYNC_CONTENT_ROOT;
use crate::storage::RemotelySaveConfig;

pub(super) fn key_to_local_path(config: &RemotelySaveConfig, key: &str) -> Result<PathBuf> {
    let key = strip_remote_prefix(key.trim_start_matches('/'), &config.remote_prefix);
    let mut rel = PathBuf::new();

    for part in key.split('/') {
        if part.is_empty() {
            continue;
        }
        rel.push(part);
    }

    if rel.as_os_str().is_empty() {
        return Err(anyhow!("empty object key"));
    }

    let mut normalized = PathBuf::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("object key escapes vault: {}", key));
            }
        }
    }

    Ok(Path::new(SYNC_CONTENT_ROOT).join(normalized))
}

pub(super) fn is_internal_marker(key: &str) -> bool {
    key.split('/').any(|part| {
        matches!(
            part,
            ".obsidian"
                | ".rr_sync_status"
                | ".rr_sync_status.tmp"
                | ".rr_sync_status.bak"
                | ".rr_sync_status.entries.tmp"
        ) || part.ends_with(".rrpart")
            || part.ends_with(".rrpart.meta")
    })
}

pub(super) fn encode_path_segments(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut first = true;
    for segment in path.split('/') {
        if !first {
            out.push('/');
        }
        first = false;
        percent_encode_into(segment, &mut out);
    }
    out
}

pub(super) fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    percent_encode_into(raw, &mut out);
    out
}

fn percent_encode_into(raw: &str, out: &mut String) {
    for &b in raw.as_bytes() {
        if is_unreserved(b) {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(hex_upper((b >> 4) & 0x0f));
            out.push(hex_upper(b & 0x0f));
        }
    }
}

fn strip_remote_prefix<'a>(key: &'a str, prefix: &str) -> &'a str {
    let prefix = prefix.trim_start_matches('/');
    if prefix.is_empty() {
        return key;
    }
    let prefix = prefix.trim_end_matches('/');
    if key == prefix {
        ""
    } else if key.starts_with(prefix) && key.as_bytes().get(prefix.len()) == Some(&b'/') {
        &key[prefix.len() + 1..]
    } else {
        key
    }
}

fn is_unreserved(b: u8) -> bool {
    matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~')
}

fn hex_upper(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        _ => char::from(b'A' + (nibble - 10)),
    }
}
