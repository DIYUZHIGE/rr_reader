use crate::storage::RemotelySaveConfig;
use crate::time;
use anyhow::{anyhow, Result};
use esp_idf_svc::http::client::{Configuration as HttpConfiguration, EspHttpConnection};
use esp_idf_svc::http::Method;
use log::{info, warn};
use std::fs;
use std::path::{Component, Path, PathBuf};

const VAULT_ROOT: &str = "/sdcard/vault";
const SYNC_STATUS_PATH: &str = "/sdcard/vault/.rr_sync_status";

unsafe extern "C" fn attach_crt_bundle(conf: *mut core::ffi::c_void) -> i32 {
    esp_idf_svc::sys::esp_crt_bundle_attach(conf)
}

#[derive(Clone, Debug)]
pub struct SyncReport {
    pub downloaded_files: usize,
    pub skipped_files: usize,
    pub status_path: String,
}

pub fn sync_vault_from_s3_config(config: &RemotelySaveConfig) -> Result<SyncReport> {
    validate_config(config)?;

    let list_url = build_list_url(config)?;
    info!("Listing remote objects: {}", list_url);
    let list_xml = http_get_text(&list_url)?;
    let keys = parse_list_keys(&list_xml);

    let mut downloaded = 0usize;
    let mut skipped = 0usize;

    for key in keys {
        if key.ends_with('/') {
            skipped += 1;
            continue;
        }

        if is_internal_marker(&key) {
            skipped += 1;
            continue;
        }

        let object_url = build_object_url(config, &key)?;
        match http_get_bytes(&object_url) {
            Ok(bytes) => {
                let target = key_to_local_path(&key)?;
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| anyhow!("mkdir {:?}: {}", parent, e))?;
                }
                fs::write(&target, bytes).map_err(|e| anyhow!("write {:?}: {}", target, e))?;
                downloaded += 1;
            }
            Err(e) => {
                warn!("Download failed for {}: {}", key, e);
                skipped += 1;
            }
        }
    }

    write_status_file(config, downloaded, skipped)?;

    Ok(SyncReport {
        downloaded_files: downloaded,
        skipped_files: skipped,
        status_path: SYNC_STATUS_PATH.to_string(),
    })
}

fn validate_config(config: &RemotelySaveConfig) -> Result<()> {
    if !config.endpoint.starts_with("http://") && !config.endpoint.starts_with("https://") {
        return Err(anyhow!("endpoint must start with http:// or https://"));
    }

    if config.region.is_empty() {
        return Err(anyhow!("region cannot be empty"));
    }

    if config.bucket_name.is_empty() {
        return Err(anyhow!("bucket_name cannot be empty"));
    }

    Ok(())
}

fn build_list_url(config: &RemotelySaveConfig) -> Result<String> {
    let base = object_base_url(config)?;
    let mut url = format!("{}?list-type=2", base);
    if !config.remote_prefix.is_empty() {
        url.push_str("&prefix=");
        url.push_str(&percent_encode(&config.remote_prefix));
    }
    Ok(url)
}

fn build_object_url(config: &RemotelySaveConfig, key: &str) -> Result<String> {
    let base = object_base_url(config)?;
    Ok(format!("{}/{}", base, encode_path_segments(key)))
}

fn object_base_url(config: &RemotelySaveConfig) -> Result<String> {
    let endpoint = config.endpoint.trim_end_matches('/');

    if config.force_path_style {
        return Ok(format!("{}/{}", endpoint, config.bucket_name));
    }

    let scheme_sep = endpoint
        .find("://")
        .ok_or_else(|| anyhow!("invalid endpoint: missing scheme"))?;
    let scheme = &endpoint[..scheme_sep];
    let host = &endpoint[scheme_sep + 3..];

    if host.is_empty() {
        return Err(anyhow!("invalid endpoint host"));
    }

    Ok(format!("{}://{}.{}", scheme, config.bucket_name, host))
}

fn parse_list_keys(xml: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = xml;

    loop {
        let Some(start) = rest.find("<Key>") else {
            break;
        };
        let after_start = &rest[start + 5..];
        let Some(end) = after_start.find("</Key>") else {
            break;
        };

        let encoded = &after_start[..end];
        let decoded = xml_unescape(encoded);
        if !decoded.is_empty() {
            keys.push(decoded);
        }

        rest = &after_start[end + 6..];
    }

    keys
}

fn http_get_text(url: &str) -> Result<String> {
    let mut bytes = http_get_bytes(url)?;
    String::from_utf8(std::mem::take(&mut bytes)).map_err(|e| anyhow!("utf8 decode {}: {}", url, e))
}

fn http_get_bytes(url: &str) -> Result<Vec<u8>> {
    let config = HttpConfiguration {
        timeout: Some(core::time::Duration::from_secs(30)),
        use_global_ca_store: true,
        crt_bundle_attach: Some(attach_crt_bundle),
        ..Default::default()
    };
    let mut connection = EspHttpConnection::new(&config)?;

    let headers: [(&str, &str); 0] = [];
    connection.initiate_request(Method::Get, url, &headers)?;
    connection.initiate_response()?;

    let status = connection.status();
    if !(200..300).contains(&status) {
        return Err(anyhow!("GET {} returned HTTP {}", url, status));
    }

    let mut out = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let n = connection
            .read(&mut chunk)
            .map_err(|e| anyhow!("read response {}: {}", url, e))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
    }
    Ok(out)
}

fn key_to_local_path(key: &str) -> Result<PathBuf> {
    let key = key.trim_start_matches('/');
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

    Ok(Path::new(VAULT_ROOT).join(normalized))
}

fn encode_path_segments(path: &str) -> String {
    path.split('/')
        .map(percent_encode)
        .collect::<Vec<_>>()
        .join("/")
}

fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for &b in raw.as_bytes() {
        if is_unreserved(b) {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(hex_upper((b >> 4) & 0x0f));
            out.push(hex_upper(b & 0x0f));
        }
    }
    out
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

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn is_internal_marker(key: &str) -> bool {
    key.starts_with('.') || key.ends_with(".obsidian/workspace.json")
}

fn write_status_file(config: &RemotelySaveConfig, downloaded: usize, skipped: usize) -> Result<()> {
    let now_ms = time::now_ms();
    let status = format!(
        concat!(
            "sync_status=ok\n",
            "timestamp_ms={}\n",
            "endpoint={}\n",
            "region={}\n",
            "bucket={}\n",
            "prefix={}\n",
            "force_path_style={}\n",
            "downloaded={}\n",
            "skipped={}\n"
        ),
        now_ms,
        config.endpoint,
        config.region,
        config.bucket_name,
        config.remote_prefix,
        config.force_path_style,
        downloaded,
        skipped
    );

    fs::write(SYNC_STATUS_PATH, status)
        .map_err(|e| anyhow!("write {}: {}", SYNC_STATUS_PATH, e))?;
    Ok(())
}
