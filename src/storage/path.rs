use anyhow::{anyhow, Result};
use std::path::{Component, Path, PathBuf};

pub(super) fn validated_relative_path(path: &str) -> Result<&Path> {
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

pub(super) fn asset_candidate_relative_paths(
    markdown_rel_path: &str,
    asset_path: &str,
) -> Result<Vec<PathBuf>> {
    if asset_path.contains("://") {
        return Err(anyhow!("remote image is not supported: {}", asset_path));
    }

    let asset_path = clean_asset_path(asset_path);
    let mut candidates = Vec::new();

    if asset_path.starts_with('/') {
        candidates.push(normalize_asset_path(
            &PathBuf::new(),
            asset_path.trim_start_matches('/'),
        )?);
    } else {
        let markdown = validated_relative_path(markdown_rel_path)?;
        let note_dir = markdown
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        candidates.push(normalize_asset_path(&note_dir, &asset_path)?);

        if let Ok(root_relative) = normalize_asset_path(&PathBuf::new(), &asset_path) {
            if !candidates
                .iter()
                .any(|candidate| candidate == &root_relative)
            {
                candidates.push(root_relative);
            }
        }
    }

    Ok(candidates)
}

pub(super) fn clean_asset_path(asset_path: &str) -> String {
    let asset_path = asset_path.split(['?', '#']).next().unwrap_or(asset_path);
    percent_decode_path(asset_path)
}

pub(super) fn file_name_matches(
    actual: Option<&std::ffi::OsStr>,
    expected: &std::ffi::OsStr,
) -> bool {
    if actual == Some(expected) {
        return true;
    }

    match (actual.and_then(|name| name.to_str()), expected.to_str()) {
        (Some(actual), Some(expected)) => actual.eq_ignore_ascii_case(expected),
        _ => false,
    }
}

fn normalize_asset_path(base: &Path, asset_path: &str) -> Result<PathBuf> {
    let asset = Path::new(asset_path);
    let mut normalized = PathBuf::new();
    for component in base.join(asset_path).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(anyhow!("path escapes vault: {:?}", asset));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("path escapes vault: {:?}", asset));
            }
        }
    }

    Ok(normalized)
}

fn percent_decode_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                decoded.push(high * 16 + low);
                index += 3;
                continue;
            }
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(decoded).unwrap_or_else(|_| path.to_owned())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
