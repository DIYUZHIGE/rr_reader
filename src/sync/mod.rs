use crate::storage::RemotelySaveConfig;
use crate::time;
use ::time::OffsetDateTime;
use anyhow::{anyhow, Result};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_svc::http::client::{Configuration as HttpConfiguration, EspHttpConnection};
use esp_idf_svc::http::Method;
use esp_idf_svc::sntp::{EspSntp, SyncStatus};
use hmac::{Hmac, Mac};
use log::{info, warn};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

type HmacSha256 = Hmac<Sha256>;

const VAULT_ROOT: &str = "/sdcard/vault";
const SYNC_STATUS_PATH: &str = "/sdcard/vault/.rr_sync_status";
const EMPTY_PAYLOAD_SHA256: &str = "UNSIGNED-PAYLOAD";
const LIST_MAX_KEYS: usize = 6;
const LIST_BODY_LIMIT: usize = 20 * 1024;
const ERROR_BODY_LIMIT: usize = 2 * 1024;
const OBJECT_KEY_MAX_BYTES: usize = 768;
const LOCAL_PATH_MAX_BYTES: usize = 1024;
const DOWNLOAD_TIMEOUT_SECS: u64 = 20;
const DOWNLOAD_READ_BUFFER_BYTES: usize = 1024;
const DOWNLOAD_DIRECT_MAX_BYTES: u64 = 0;
const DOWNLOAD_CHUNK_BYTES: u64 = 16 * 1024;
const DOWNLOAD_MAX_ATTEMPTS: usize = 20;
const DOWNLOAD_MIN_FREE_HEAP: u32 = 44 * 1024;
const DOWNLOAD_LOW_HEAP_RETRY_DELAY_MS: u32 = 300;

const SNTP_SERVERS: &str = "ntp.aliyun.com";

/// Cache of the last server Date so we don't HEAD OSS for every signing.
struct SyncUnsafeCell<T>(core::cell::UnsafeCell<T>);
unsafe impl<T> Sync for SyncUnsafeCell<T> {}

static CACHED_SERVER_DATETIME: SyncUnsafeCell<Option<OffsetDateTime>> =
    SyncUnsafeCell(core::cell::UnsafeCell::new(None));

fn cached_server_datetime_get() -> Option<OffsetDateTime> {
    unsafe { *CACHED_SERVER_DATETIME.0.get().as_ref().unwrap() }
}

fn cached_server_datetime_set(dt: OffsetDateTime) {
    unsafe {
        *CACHED_SERVER_DATETIME.0.get() = Some(dt);
    }
}

unsafe extern "C" fn attach_crt_bundle(conf: *mut core::ffi::c_void) -> i32 {
    esp_idf_svc::sys::esp_crt_bundle_attach(conf)
}

#[derive(Clone, Debug)]
pub struct SyncReport {
    pub downloaded_files: usize,
    pub skipped_files: usize,
    pub deleted_files: usize,
    pub status_path: String,
}

struct RemoteEntry {
    key: String,
    size: u64,
    etag: String,
}

#[derive(Clone, Debug)]
struct SyncManifestEntry {
    size: u64,
    etag: String,
    local_path: String,
}

pub fn sync_vault_from_s3_config(
    config: &RemotelySaveConfig,
    on_progress: &mut dyn FnMut(&str),
) -> Result<SyncReport> {
    validate_config(config)?;
    on_progress("正在同步时间...");
    ensure_time_synced()?;

    let mut downloaded = 0usize;
    let mut skipped = 0usize;
    let mut skipped_unchanged = 0usize;

    let mut continuation_token: Option<String> = None;
    let mut page = 0usize;
    let previous_manifest = read_sync_manifest();
    let mut new_manifest: HashMap<String, SyncManifestEntry> = HashMap::new();
    let mut seen_keys: HashSet<String> = HashSet::new();

    loop {
        page += 1;
        on_progress(&format!("获取文件列表 第{}页...", page));
        let list_url = build_list_url(config, continuation_token.as_deref())?;
        info!("Listing remote objects: {}", list_url);
        let (entries, next_token) = http_list_objects_signed(config, &list_url)?;

        if entries.is_empty() && next_token.is_none() {
            break;
        }

        let total_in_page = entries.len();
        let mut processed_in_page = 0usize;

        for entry in entries {
            processed_in_page += 1;
            let key = &entry.key;

            if key.ends_with('/') {
                skipped += 1;
                continue;
            }

            if is_internal_marker(key) {
                skipped += 1;
                continue;
            }

            if key.len() > OBJECT_KEY_MAX_BYTES {
                warn!(
                    "Skipping file with very long object key ({} bytes): {}",
                    key.len(),
                    key
                );
                skipped += 1;
                continue;
            }

            // ESP32 FAT driver rejects filenames longer than ~200 chars
            if key.rsplit('/').next().unwrap_or(key.as_str()).len() > 200 {
                warn!(
                    "Skipping file with very long name ({} chars): {}",
                    key.len(),
                    key
                );
                skipped += 1;
                continue;
            }

            let target = key_to_local_path(config, key)?;
            let target_str = target.to_string_lossy().to_string();
            if target_str.len() > LOCAL_PATH_MAX_BYTES {
                warn!(
                    "Skipping file with very long local path ({} bytes): {}",
                    target_str.len(),
                    target_str
                );
                skipped += 1;
                continue;
            }
            seen_keys.insert(key.to_string());

            // Skip only when both local size and the previous ETag match the remote entry.
            // Size-only comparisons can miss changed files with identical byte length.
            if let Ok(meta) = fs::metadata(&target) {
                let manifest_matches = previous_manifest
                    .get(key)
                    .map(|old| {
                        old.size == entry.size && !old.etag.is_empty() && old.etag == entry.etag
                    })
                    .unwrap_or(false);
                let legacy_size_only_match = entry.etag.is_empty() && meta.len() == entry.size;
                if meta.len() == entry.size && (manifest_matches || legacy_size_only_match) {
                    skipped_unchanged += 1;
                    new_manifest.insert(
                        key.to_string(),
                        SyncManifestEntry {
                            size: entry.size,
                            etag: entry.etag.clone(),
                            local_path: target_str.clone(),
                        },
                    );
                    continue;
                }
            }

            // Show progress every 10 files or on last file in page
            if processed_in_page % 10 == 0 || processed_in_page == total_in_page {
                let name = key.rsplit('/').next().unwrap_or(key);
                on_progress(&format!(
                    "下载: {} ({}/{})",
                    name,
                    downloaded + 1,
                    processed_in_page
                ));
            }

            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| anyhow!("mkdir {:?}: {}", parent, e))?;
            }
            let object_url = build_object_url(config, key)?;
            match download_file_signed(config, &object_url, &target_str, entry.size) {
                Ok(()) => {
                    downloaded += 1;
                    new_manifest.insert(
                        key.to_string(),
                        SyncManifestEntry {
                            size: entry.size,
                            etag: entry.etag.clone(),
                            local_path: target_str.clone(),
                        },
                    );
                }
                Err(e) => {
                    warn!("Download failed for {}: {}", key, e);
                    skipped += 1;
                    // Brief pause to let stack recover
                    FreeRtos::delay_ms(200);
                }
            }
        }

        continuation_token = next_token;
        if continuation_token.is_none() {
            break;
        }
    }

    if skipped_unchanged > 0 {
        info!("Skipped {} files (unchanged)", skipped_unchanged);
    }

    let deleted_stale = delete_stale_manifest_files(&previous_manifest, &seen_keys)?;
    if deleted_stale > 0 {
        info!("Deleted {} stale files from previous sync", deleted_stale);
    }

    on_progress("正在写入状态文件...");
    write_status_file(config, downloaded, skipped, deleted_stale, &new_manifest)?;

    Ok(SyncReport {
        downloaded_files: downloaded,
        skipped_files: skipped,
        deleted_files: deleted_stale,
        status_path: SYNC_STATUS_PATH.to_string(),
    })
}

fn ensure_time_synced() -> Result<()> {
    let now_before = OffsetDateTime::now_utc();
    if now_before.year() >= 2024 {
        return Ok(());
    }

    let conf = esp_idf_svc::sntp::SntpConf {
        servers: [SNTP_SERVERS],
        ..Default::default()
    };
    let sntp = EspSntp::new(&conf).map_err(|e| anyhow!("SNTP init failed: {}", e))?;

    let mut completed = false;
    for _ in 0..120 {
        match sntp.get_sync_status() {
            SyncStatus::Completed => {
                completed = true;
                break;
            }
            SyncStatus::InProgress | SyncStatus::Reset => {
                FreeRtos::delay_ms(250);
            }
        }
    }

    let now_after = OffsetDateTime::now_utc();
    if completed && now_after.year() >= 2024 {
        return Ok(());
    }

    if now_after.year() >= 2024 {
        warn!(
            "SNTP did not report completed, but clock is already valid (year={})",
            now_after.year()
        );
        return Ok(());
    }

    warn!("SNTP sync timeout; will fallback to server Date header for signing");
    Ok(())
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

    if config.access_key_id.is_empty() || config.secret_access_key.is_empty() {
        return Err(anyhow!("access key is missing"));
    }

    Ok(())
}

fn build_list_url(config: &RemotelySaveConfig, continuation_token: Option<&str>) -> Result<String> {
    let base = object_base_url(config)?;
    let mut url = format!("{}/?list-type=2&max-keys={}", base, LIST_MAX_KEYS);
    if !config.remote_prefix.is_empty() {
        url.push_str("&prefix=");
        url.push_str(&percent_encode(&config.remote_prefix));
    }
    if let Some(token) = continuation_token {
        url.push_str("&continuation-token=");
        url.push_str(&percent_encode(token));
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

fn parse_list_response(xml: &str) -> (Vec<RemoteEntry>, Option<String>) {
    let mut entries = Vec::new();
    let mut next_token = None;
    let mut rest = xml;

    // Parse <Contents> blocks
    loop {
        let Some(contents_start) = rest.find("<Contents>") else {
            break;
        };
        let after_cs = &rest[contents_start + 10..];
        let Some(contents_end) = after_cs.find("</Contents>") else {
            break;
        };
        let block = &after_cs[..contents_end];

        // Extract <Key>
        let key = extract_xml_text_owned(block, "<Key>", "</Key>").unwrap_or_default();
        // Extract <Size>
        let size_str = extract_xml_text_ref(block, "<Size>", "</Size>").unwrap_or("0");
        let size: u64 = size_str.parse().unwrap_or(0);
        let etag = extract_xml_text_owned(block, "<ETag>", "</ETag>")
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();

        if !key.is_empty() {
            entries.push(RemoteEntry { key, size, etag });
        }

        rest = &after_cs[contents_end + 12..];
    }

    // Parse NextContinuationToken
    if let Some(start) = xml.find("<NextContinuationToken>") {
        let after_start = &xml[start + 23..];
        if let Some(end) = after_start.find("</NextContinuationToken>") {
            let token = xml_unescape(&after_start[..end]);
            if !token.is_empty() {
                next_token = Some(token);
            }
        }
    }

    (entries, next_token)
}

fn extract_xml_text_ref<'a>(xml: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = xml.find(open)?;
    let after = &xml[start + open.len()..];
    let end = after.find(close)?;
    Some(&after[..end])
}

fn extract_xml_text_owned(xml: &str, open: &str, close: &str) -> Option<String> {
    extract_xml_text_ref(xml, open, close).map(xml_unescape)
}

const XML_CONTENTS_OPEN: &[u8] = b"<Contents>";
const XML_CONTENTS_CLOSE: &[u8] = b"</Contents>";
const XML_KEY_OPEN: &[u8] = b"<Key>";
const XML_KEY_CLOSE: &[u8] = b"</Key>";
const XML_SIZE_OPEN: &[u8] = b"<Size>";
const XML_SIZE_CLOSE: &[u8] = b"</Size>";
const XML_ETAG_OPEN: &[u8] = b"<ETag>";
const XML_ETAG_CLOSE: &[u8] = b"</ETag>";
const XML_NEXT_TOKEN_OPEN: &[u8] = b"<NextContinuationToken>";
const XML_NEXT_TOKEN_CLOSE: &[u8] = b"</NextContinuationToken>";
const XML_STREAM_BUFFER_LIMIT: usize = 8 * 1024;

struct ListXmlStreamParser {
    buf: Vec<u8>,
    entries: Vec<RemoteEntry>,
    next_token: Option<String>,
}

impl ListXmlStreamParser {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(2048),
            entries: Vec::new(),
            next_token: None,
        }
    }

    fn push(&mut self, incoming: &[u8]) -> Result<()> {
        self.buf.extend_from_slice(incoming);
        self.consume_complete_blocks();
        self.capture_next_token_if_present();
        self.trim_prefix();

        if self.buf.len() > XML_STREAM_BUFFER_LIMIT {
            return Err(anyhow!(
                "list XML parser buffer exceeded {} bytes",
                XML_STREAM_BUFFER_LIMIT
            ));
        }

        Ok(())
    }

    fn finish(mut self) -> Result<(Vec<RemoteEntry>, Option<String>)> {
        self.consume_complete_blocks();
        self.capture_next_token_if_present();
        Ok((self.entries, self.next_token))
    }

    fn consume_complete_blocks(&mut self) {
        loop {
            let Some(start) = find_subslice(&self.buf, XML_CONTENTS_OPEN) else {
                break;
            };
            let after_start = start + XML_CONTENTS_OPEN.len();
            let Some(rel_end) = find_subslice(&self.buf[after_start..], XML_CONTENTS_CLOSE) else {
                break;
            };
            let end = after_start + rel_end;

            let block = &self.buf[after_start..end];
            let key = extract_xml_text_bytes(block, XML_KEY_OPEN, XML_KEY_CLOSE)
                .map(|s| xml_unescape(&s))
                .unwrap_or_default();
            let size = extract_xml_text_bytes(block, XML_SIZE_OPEN, XML_SIZE_CLOSE)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let etag = extract_xml_text_bytes(block, XML_ETAG_OPEN, XML_ETAG_CLOSE)
                .map(|s| xml_unescape(&s).trim_matches('"').to_string())
                .unwrap_or_default();

            if !key.is_empty() {
                self.entries.push(RemoteEntry { key, size, etag });
            }

            let consume_end = end + XML_CONTENTS_CLOSE.len();
            self.buf.drain(..consume_end);
        }
    }

    fn capture_next_token_if_present(&mut self) {
        if self.next_token.is_some() {
            return;
        }

        let Some(start) = find_subslice(&self.buf, XML_NEXT_TOKEN_OPEN) else {
            return;
        };
        let after_start = start + XML_NEXT_TOKEN_OPEN.len();
        let Some(rel_end) = find_subslice(&self.buf[after_start..], XML_NEXT_TOKEN_CLOSE) else {
            return;
        };
        let end = after_start + rel_end;
        let token_raw = String::from_utf8_lossy(&self.buf[after_start..end]).to_string();
        let token = xml_unescape(&token_raw);
        if !token.is_empty() {
            self.next_token = Some(token);
        }
    }

    fn trim_prefix(&mut self) {
        if self.buf.len() <= XML_STREAM_BUFFER_LIMIT / 2 {
            return;
        }

        if let Some(pos) = find_subslice(&self.buf, XML_CONTENTS_OPEN) {
            if pos > 0 {
                self.buf.drain(..pos);
            }
            return;
        }

        if let Some(pos) = find_subslice(&self.buf, XML_NEXT_TOKEN_OPEN) {
            if pos > 0 {
                self.buf.drain(..pos);
            }
            return;
        }

        let keep = XML_STREAM_BUFFER_LIMIT / 4;
        if self.buf.len() > keep {
            let drop_len = self.buf.len() - keep;
            self.buf.drain(..drop_len);
        }
    }
}

fn extract_xml_text_bytes(haystack: &[u8], open: &[u8], close: &[u8]) -> Option<String> {
    let start = find_subslice(haystack, open)? + open.len();
    let rel_end = find_subslice(&haystack[start..], close)?;
    let end = start + rel_end;
    Some(String::from_utf8_lossy(&haystack[start..end]).to_string())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn http_get_text_signed(config: &RemotelySaveConfig, url: &str) -> Result<String> {
    let bytes = http_get_bytes_signed_limited(config, url, LIST_BODY_LIMIT)?;
    String::from_utf8(bytes).map_err(|e| anyhow!("utf8 decode {}: {}", url, e))
}

fn http_list_objects_signed(
    config: &RemotelySaveConfig,
    url: &str,
) -> Result<(Vec<RemoteEntry>, Option<String>)> {
    let parsed = parse_url(url)?;
    let candidates = signing_candidates(config, &parsed.host);
    let timestamp = resolve_signing_timestamp(url)?;

    let mut last_error: Option<anyhow::Error> = None;
    for candidate in candidates {
        match http_list_objects_signed_once(config, url, &parsed, &candidate, &timestamp) {
            Ok(result) => return Ok(result),
            Err(e) => {
                warn!(
                    "Signed list GET failed with service={} region={}: {}",
                    candidate.service, candidate.region, e
                );
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("signed list GET failed")))
}

fn http_list_objects_signed_once(
    config: &RemotelySaveConfig,
    url: &str,
    parsed: &ParsedUrl,
    candidate: &SigningCandidate,
    timestamp: &SigningTimestamp,
) -> Result<(Vec<RemoteEntry>, Option<String>)> {
    let signing = signing_material(config, parsed, "GET", candidate, timestamp, &[])?;

    let config_http = HttpConfiguration {
        buffer_size: Some(DOWNLOAD_READ_BUFFER_BYTES),
        buffer_size_tx: Some(1024),
        timeout: Some(core::time::Duration::from_secs(30)),
        use_global_ca_store: true,
        crt_bundle_attach: Some(attach_crt_bundle),
        ..Default::default()
    };
    wait_for_download_heap("list GET request init");
    let mut connection = EspHttpConnection::new(&config_http)?;

    let headers = [
        ("Host", parsed.host.as_str()),
        ("x-oss-date", signing.request_date.as_str()),
        ("x-oss-content-sha256", signing.payload_hash.as_str()),
        ("Authorization", signing.authorization.as_str()),
        ("Accept-Encoding", "identity"),
    ];

    connection.initiate_request(Method::Get, url, &headers)?;
    connection.initiate_response()?;

    let status = connection.status();
    if !(200..300).contains(&status) {
        let body = read_http_body_limited(&mut connection, url, ERROR_BODY_LIMIT)?;
        let body_preview = String::from_utf8_lossy(&body);
        return Err(anyhow!(
            "list GET {} returned HTTP {} body={}",
            url,
            status,
            truncate_debug_text(&body_preview, 1200)
        ));
    }

    let mut parser = ListXmlStreamParser::new();
    let mut chunk = [0u8; DOWNLOAD_READ_BUFFER_BYTES];
    loop {
        let n = connection
            .read(&mut chunk)
            .map_err(|e| anyhow!("read list response {}: {}", url, e))?;
        if n == 0 {
            break;
        }
        parser.push(&chunk[..n])?;
    }

    parser.finish()
}

fn http_get_bytes_signed_limited(
    config: &RemotelySaveConfig,
    url: &str,
    max_body_bytes: usize,
) -> Result<Vec<u8>> {
    let parsed = parse_url(url)?;
    let candidates = signing_candidates(config, &parsed.host);
    let timestamp = resolve_signing_timestamp(url)?;

    let mut last_error: Option<anyhow::Error> = None;
    for candidate in candidates {
        match http_get_bytes_signed_once(
            config,
            url,
            &parsed,
            &candidate,
            &timestamp,
            max_body_bytes,
        ) {
            Ok(bytes) => return Ok(bytes),
            Err(e) => {
                warn!(
                    "Signed GET failed with service={} region={}: {}",
                    candidate.service, candidate.region, e
                );
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("signed GET failed")))
}

/// Stream a signed GET response directly to a file in small signed Range
/// chunks. Each chunk closes its TLS connection, which is slower but much
/// more robust on ESP32-C3 than one long-lived TLS read.
#[derive(Clone, Copy, Debug)]
struct DownloadChunkResult {
    bytes_written: u64,
    completed: bool,
}

fn download_file_signed(
    config: &RemotelySaveConfig,
    url: &str,
    file_path: &str,
    expected_size: u64,
) -> Result<()> {
    let parsed = parse_url(url)?;
    let candidates = signing_candidates(config, &parsed.host);
    let timestamp = resolve_signing_timestamp(url)?;
    let temp_path = temp_download_path(file_path);

    if expected_size == 0 {
        fs::write(&temp_path, [])
            .map_err(|e| anyhow!("write empty temp {:?}: {}", temp_path, e))?;
        finalize_temp_download(&temp_path, file_path, expected_size)?;
        return Ok(());
    }

    if expected_size <= DOWNLOAD_DIRECT_MAX_BYTES {
        match download_file_signed_direct(
            config,
            url,
            &parsed,
            &candidates,
            &timestamp,
            expected_size,
            &temp_path,
        ) {
            Ok(()) => {
                finalize_temp_download(&temp_path, file_path, expected_size)?;
                return Ok(());
            }
            Err(e) => {
                warn!(
                    "Direct download failed for {}; falling back to ranged resume: {}",
                    url, e
                );
                let _ = fs::remove_file(&temp_path);
            }
        }
    }

    for attempt in 1..=DOWNLOAD_MAX_ATTEMPTS {
        match resume_offset(&temp_path, expected_size) {
            Ok(done) if expected_size > 0 && done == expected_size => {
                finalize_temp_download(&temp_path, file_path, expected_size)?;
                return Ok(());
            }
            Ok(done) => {
                info!(
                    "Download progress before attempt {}/{}: {} / {} bytes for {}",
                    attempt, DOWNLOAD_MAX_ATTEMPTS, done, expected_size, file_path
                );
            }
            Err(e) => return Err(e),
        }

        let mut last_error: Option<anyhow::Error> = None;
        let mut made_progress = false;
        for candidate in &candidates {
            match download_file_signed_chunk(
                config,
                url,
                &parsed,
                candidate,
                &timestamp,
                expected_size,
                &temp_path,
            ) {
                Ok(chunk_result) => {
                    if chunk_result.completed {
                        finalize_temp_download(&temp_path, file_path, expected_size)?;
                        return Ok(());
                    }
                    made_progress = chunk_result.bytes_written > 0;
                    last_error = None;
                    break;
                }
                Err(e) => {
                    let err_msg = format!("{}", e);
                    if err_msg.contains("ERROR")
                        || err_msg.contains("ESP_FAIL")
                        || err_msg.contains("No more processes")
                        || err_msg.contains("timeout")
                    {
                        warn!(
                            "Download attempt {}/{} failed (transient): {}",
                            attempt, DOWNLOAD_MAX_ATTEMPTS, err_msg
                        );
                    } else {
                        warn!(
                            "Download attempt {}/{} failed: {}",
                            attempt, DOWNLOAD_MAX_ATTEMPTS, err_msg
                        );
                    }
                    last_error = Some(e);
                }
            }
        }

        let delay_ms = if made_progress {
            1000
        } else {
            1000 * (1u32 << ((attempt - 1).min(5) as u32))
        };
        FreeRtos::delay_ms(delay_ms);

        if attempt == DOWNLOAD_MAX_ATTEMPTS {
            return Err(last_error.unwrap_or_else(|| {
                anyhow!(
                    "download did not complete after {} attempts; partial kept at {:?}",
                    DOWNLOAD_MAX_ATTEMPTS,
                    temp_path
                )
            }));
        }
    }

    Err(anyhow!("unreachable"))
}

fn download_file_signed_direct(
    config: &RemotelySaveConfig,
    url: &str,
    parsed: &ParsedUrl,
    candidates: &[SigningCandidate],
    timestamp: &SigningTimestamp,
    expected_size: u64,
    temp_path: &Path,
) -> Result<()> {
    let mut last_error: Option<anyhow::Error> = None;

    for candidate in candidates {
        match download_file_signed_direct_once(
            config,
            url,
            parsed,
            candidate,
            timestamp,
            expected_size,
            temp_path,
        ) {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!(
                    "Direct GET failed with service={} region={}: {}",
                    candidate.service, candidate.region, e
                );
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("direct download failed")))
}

fn download_file_signed_direct_once(
    config: &RemotelySaveConfig,
    url: &str,
    parsed: &ParsedUrl,
    candidate: &SigningCandidate,
    timestamp: &SigningTimestamp,
    expected_size: u64,
    temp_path: &Path,
) -> Result<()> {
    let signing = signing_material(config, parsed, "GET", candidate, timestamp, &[])?;

    let config_http = HttpConfiguration {
        buffer_size: Some(DOWNLOAD_READ_BUFFER_BYTES),
        buffer_size_tx: Some(1024),
        timeout: Some(core::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS)),
        use_global_ca_store: true,
        crt_bundle_attach: Some(attach_crt_bundle),
        ..Default::default()
    };
    wait_for_download_heap("direct GET request init");
    let mut connection = EspHttpConnection::new(&config_http)?;

    let headers = [
        ("Host", parsed.host.as_str()),
        ("x-oss-date", signing.request_date.as_str()),
        ("x-oss-content-sha256", signing.payload_hash.as_str()),
        ("Authorization", signing.authorization.as_str()),
        ("Accept-Encoding", "identity"),
    ];

    connection.initiate_request(Method::Get, url, &headers)?;
    connection.initiate_response()?;

    let status = connection.status();
    if !(200..300).contains(&status) {
        let body = read_http_body_limited(&mut connection, url, ERROR_BODY_LIMIT)?;
        let body_preview = String::from_utf8_lossy(&body);
        return Err(anyhow!(
            "direct GET {} returned HTTP {} body={}",
            url,
            status,
            truncate_debug_text(&body_preview, 2000)
        ));
    }

    if let Some(content_length) = connection.header("Content-Length") {
        if let Ok(content_length) = content_length.parse::<u64>() {
            if content_length != expected_size {
                warn!(
                    "Content-Length mismatch before direct download: url={} expected={} header={}",
                    url, expected_size, content_length
                );
            }
        }
    }

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(temp_path)
        .map_err(|e| anyhow!("open temp {:?}: {}", temp_path, e))?;

    let mut chunk = [0u8; DOWNLOAD_READ_BUFFER_BYTES];
    let mut written = 0u64;
    loop {
        let n = connection
            .read(&mut chunk)
            .map_err(|e| anyhow!("read response {} after {} bytes: {}", url, written, e))?;
        if n == 0 {
            break;
        }
        file.write_all(&chunk[..n])
            .map_err(|e| anyhow!("write temp {:?} after {} bytes: {}", temp_path, written, e))?;
        written += n as u64;
        if written > expected_size {
            return Err(anyhow!(
                "direct download overshot for {}: expected {} bytes, wrote {} bytes",
                url,
                expected_size,
                written
            ));
        }
    }

    file.flush()
        .map_err(|e| anyhow!("flush temp {:?}: {}", temp_path, e))?;

    if written != expected_size {
        return Err(anyhow!(
            "direct download size mismatch for {}: expected {} bytes, wrote {} bytes",
            url,
            expected_size,
            written
        ));
    }

    info!("Direct downloaded {} bytes for {}", written, url);
    Ok(())
}

fn download_file_signed_chunk(
    config: &RemotelySaveConfig,
    url: &str,
    parsed: &ParsedUrl,
    candidate: &SigningCandidate,
    timestamp: &SigningTimestamp,
    expected_size: u64,
    temp_path: &Path,
) -> Result<DownloadChunkResult> {
    let start = resume_offset(temp_path, expected_size)?;
    if expected_size > 0 && start == expected_size {
        return Ok(DownloadChunkResult {
            bytes_written: 0,
            completed: true,
        });
    }

    let end = if expected_size > 0 {
        (start + DOWNLOAD_CHUNK_BYTES - 1).min(expected_size - 1)
    } else {
        start + DOWNLOAD_CHUNK_BYTES - 1
    };
    let range_header = format!("bytes={}-{}", start, end);
    let extra_signing_headers = [("range", range_header.as_str())];
    let signing = signing_material(
        config,
        parsed,
        "GET",
        candidate,
        timestamp,
        &extra_signing_headers,
    )?;

    let config_http = HttpConfiguration {
        buffer_size: Some(DOWNLOAD_READ_BUFFER_BYTES),
        buffer_size_tx: Some(1024),
        timeout: Some(core::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS)),
        use_global_ca_store: true,
        crt_bundle_attach: Some(attach_crt_bundle),
        ..Default::default()
    };
    wait_for_download_heap("range GET request init");
    let mut connection = EspHttpConnection::new(&config_http)?;

    let headers = [
        ("Host", parsed.host.as_str()),
        ("x-oss-date", signing.request_date.as_str()),
        ("x-oss-content-sha256", signing.payload_hash.as_str()),
        ("Authorization", signing.authorization.as_str()),
        ("Range", range_header.as_str()),
        ("Accept-Encoding", "identity"),
    ];

    connection.initiate_request(Method::Get, url, &headers)?;
    info!("Chunk req: {}-{} for {}", start, end, url);
    connection.initiate_response()?;

    let status = connection.status();
    let mut written = start;
    let mut append = true;

    if status == 200 {
        warn!(
            "Server ignored Range for {}; restarting download from byte 0",
            url
        );
        let _ = fs::remove_file(temp_path);
        written = 0;
        append = false;
    } else if status != 206 {
        let body = read_http_body_limited(&mut connection, url, ERROR_BODY_LIMIT)?;
        let body_preview = String::from_utf8_lossy(&body);
        return Err(anyhow!(
            "range GET {} from byte {} returned HTTP {} body={}",
            url,
            start,
            status,
            truncate_debug_text(&body_preview, 2000)
        ));
    } else {
        info!("Chunk rsp: {}-{} 206 h={}", start, end, free_heap());
    }

    if let Some(content_length) = connection.header("Content-Length") {
        if let Ok(content_length) = content_length.parse::<u64>() {
            let expected_response_len = if status == 206 {
                end.saturating_sub(start) + 1
            } else if expected_size > written {
                expected_size - written
            } else {
                expected_size
            };
            if expected_response_len > 0 && content_length != expected_response_len {
                warn!(
                    "Content-Length mismatch before chunk: url={} expected_response={} header={} already_written={}",
                    url, expected_response_len, content_length, written
                );
            }
        }
    }

    let mut open_options = fs::OpenOptions::new();
    open_options.write(true).create(true);
    if append && written > 0 {
        open_options.append(true);
    } else {
        open_options.truncate(true);
    }
    let mut file = match open_options.open(temp_path) {
        Ok(f) => f,
        Err(e) => return Err(anyhow!("open temp {:?}: {}", temp_path, e)),
    };

    let mut chunk = [0u8; DOWNLOAD_READ_BUFFER_BYTES];
    let mut bytes_this_chunk = 0u64;
    loop {
        let n = match connection.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                return Err(anyhow!(
                    "read response {} after {} bytes: {}",
                    url,
                    written,
                    e
                ));
            }
        };
        if status == 206 && bytes_this_chunk + n as u64 > DOWNLOAD_CHUNK_BYTES {
            return Err(anyhow!(
                "server sent more than requested chunk for {}: chunk={} incoming={}",
                url,
                bytes_this_chunk,
                n
            ));
        }
        if let Err(e) = file.write_all(&chunk[..n]) {
            return Err(anyhow!(
                "write temp {:?} after {} bytes: {}",
                temp_path,
                written,
                e
            ));
        }
        bytes_this_chunk += n as u64;
        written += n as u64;
    }

    if let Err(e) = file.flush() {
        return Err(anyhow!("flush temp {:?}: {}", temp_path, e));
    }
    drop(file);

    if expected_size > 0 && written > expected_size {
        return Err(anyhow!(
            "download overshot for {}: expected {} bytes, wrote {} bytes",
            url,
            expected_size,
            written
        ));
    }

    Ok(DownloadChunkResult {
        bytes_written: bytes_this_chunk,
        completed: expected_size > 0 && written == expected_size,
    })
}

fn finalize_temp_download(temp_path: &Path, file_path: &str, expected_size: u64) -> Result<()> {
    let written = fs::metadata(temp_path)
        .map_err(|e| anyhow!("stat complete temp {:?}: {}", temp_path, e))?
        .len();
    if expected_size > 0 && written != expected_size {
        return Err(anyhow!(
            "complete temp size mismatch for {:?}: expected {} bytes, got {} bytes",
            temp_path,
            expected_size,
            written
        ));
    }

    let _ = fs::remove_file(file_path);
    fs::rename(temp_path, file_path)
        .map_err(|e| anyhow!("rename {:?} -> {:?}: {}", temp_path, file_path, e))?;
    info!("Downloaded {} bytes to {}", written, file_path);
    Ok(())
}

fn temp_download_path(file_path: &str) -> PathBuf {
    let path = Path::new(file_path);
    let mut name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    name.push_str(".rrpart");

    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

fn resume_offset(temp_path: &Path, expected_size: u64) -> Result<u64> {
    let Ok(meta) = fs::metadata(temp_path) else {
        return Ok(0);
    };

    let len = meta.len();
    if len == 0 {
        let _ = fs::remove_file(temp_path);
        return Ok(0);
    }

    if expected_size > 0 && len > expected_size {
        warn!(
            "Discarding oversized partial download {:?}: partial={} expected={}",
            temp_path, len, expected_size
        );
        let _ = fs::remove_file(temp_path);
        return Ok(0);
    }

    Ok(len)
}

fn free_heap() -> u32 {
    unsafe { esp_idf_hal::sys::esp_get_free_heap_size() }
}

fn wait_for_download_heap(tag: &str) {
    for _ in 0..2 {
        let heap = free_heap();
        if heap >= DOWNLOAD_MIN_FREE_HEAP {
            return;
        }
        warn!(
            "Low heap before {}: {} bytes (< {}), brief wait",
            tag, heap, DOWNLOAD_MIN_FREE_HEAP
        );
        FreeRtos::delay_ms(DOWNLOAD_LOW_HEAP_RETRY_DELAY_MS);
    }
}

fn http_get_bytes_signed_once(
    config: &RemotelySaveConfig,
    url: &str,
    parsed: &ParsedUrl,
    candidate: &SigningCandidate,
    timestamp: &SigningTimestamp,
    max_body_bytes: usize,
) -> Result<Vec<u8>> {
    let signing = signing_material(config, parsed, "GET", candidate, timestamp, &[])?;

    let config_http = HttpConfiguration {
        buffer_size: Some(2048),
        buffer_size_tx: Some(1024),
        timeout: Some(core::time::Duration::from_secs(30)),
        use_global_ca_store: true,
        crt_bundle_attach: Some(attach_crt_bundle),
        ..Default::default()
    };
    wait_for_download_heap("signed GET request init");
    let mut connection = EspHttpConnection::new(&config_http)?;

    let auth_header = signing.authorization;
    let request_date = signing.request_date;
    let payload_hash = signing.payload_hash;

    let headers = [
        ("Host", parsed.host.as_str()),
        ("x-oss-date", request_date.as_str()),
        ("x-oss-content-sha256", payload_hash.as_str()),
        ("Authorization", auth_header.as_str()),
        ("Accept-Encoding", "identity"),
    ];

    connection.initiate_request(Method::Get, url, &headers)?;
    connection.initiate_response()?;

    let status = connection.status();
    let body = read_http_body_limited(&mut connection, url, max_body_bytes)?;

    if !(200..300).contains(&status) {
        let body_preview = String::from_utf8_lossy(&body);
        return Err(anyhow!(
            "GET {} returned HTTP {} (service={} region={}) scope={} creq_sha256={} sts_sha256={} body={} ",
            url,
            status,
            candidate.service,
            candidate.region,
            signing.credential_scope,
            signing.canonical_request_sha256,
            signing.string_to_sign_sha256,
            truncate_debug_text(&body_preview, 2000)
        ));
    }

    Ok(body)
}

fn read_http_body_limited(
    connection: &mut EspHttpConnection,
    url: &str,
    max_body_bytes: usize,
) -> Result<Vec<u8>> {
    let mut initial_cap = DOWNLOAD_READ_BUFFER_BYTES.min(max_body_bytes);
    if let Some(content_length) = connection.header("Content-Length") {
        if let Ok(content_length) = content_length.parse::<usize>() {
            initial_cap = content_length.min(max_body_bytes);
        }
    }

    let mut out = Vec::with_capacity(initial_cap.max(512));
    let mut chunk = [0u8; DOWNLOAD_READ_BUFFER_BYTES];
    loop {
        let n = connection
            .read(&mut chunk)
            .map_err(|e| anyhow!("read response {}: {}", url, e))?;
        if n == 0 {
            break;
        }
        if out.len() + n > max_body_bytes {
            return Err(anyhow!(
                "response {} exceeded memory limit of {} bytes",
                url,
                max_body_bytes
            ));
        }
        out.extend_from_slice(&chunk[..n]);
    }
    Ok(out)
}

fn truncate_debug_text(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out.replace('\n', " ").replace('\r', " ")
}

#[derive(Clone, Debug)]
struct SigningTimestamp {
    date_stamp: String,
    request_date: String,
}

fn resolve_signing_timestamp(url: &str) -> Result<SigningTimestamp> {
    let now = OffsetDateTime::now_utc();
    if now.year() >= 2024 {
        return Ok(signing_timestamp_from_datetime(now));
    }

    // Try the in-memory cache first to avoid extra HTTPS HEAD requests.
    if let Some(cached) = cached_server_datetime_get() {
        let age_secs = (OffsetDateTime::now_utc() - cached).whole_seconds().abs();
        if age_secs < 60 {
            return Ok(signing_timestamp_from_datetime(cached));
        }
    }

    if let Some(server_dt) = fetch_server_datetime(url)? {
        cached_server_datetime_set(server_dt);
        return Ok(signing_timestamp_from_datetime(server_dt));
    }

    if let Some(cached) = cached_server_datetime_get() {
        return Ok(signing_timestamp_from_datetime(cached));
    }

    Err(anyhow!(
        "clock invalid and failed to fetch server Date header"
    ))
}

fn signing_timestamp_from_datetime(dt: OffsetDateTime) -> SigningTimestamp {
    let month: u8 = dt.month().into();
    let date_stamp = format!("{:04}{:02}{:02}", dt.year(), month, dt.day());
    let request_date = format!(
        "{}T{:02}{:02}{:02}Z",
        date_stamp,
        dt.hour(),
        dt.minute(),
        dt.second()
    );
    SigningTimestamp {
        date_stamp,
        request_date,
    }
}

fn fetch_server_datetime(url: &str) -> Result<Option<OffsetDateTime>> {
    let config_http = HttpConfiguration {
        buffer_size: Some(4096),
        buffer_size_tx: Some(2048),
        timeout: Some(core::time::Duration::from_secs(15)),
        use_global_ca_store: true,
        crt_bundle_attach: Some(attach_crt_bundle),
        ..Default::default()
    };
    let mut connection = EspHttpConnection::new(&config_http)?;
    let headers: [(&str, &str); 0] = [];
    connection.initiate_request(Method::Head, url, &headers)?;
    info!("HEAD Date req: {}", url);
    connection.initiate_response()?;

    let server_date = connection.header("Date").map(|s| s.to_string());

    match server_date {
        Some(value) => parse_http_date(&value).map(Some),
        None => Ok(None),
    }
}

fn parse_http_date(date: &str) -> Result<OffsetDateTime> {
    // Example: Sat, 02 May 2026 12:34:56 GMT
    let parts: Vec<&str> = date.split_whitespace().collect();
    if parts.len() < 6 {
        return Err(anyhow!("invalid Date header: {}", date));
    }

    let day: u8 = parts[1]
        .trim_end_matches(',')
        .parse()
        .map_err(|_| anyhow!("invalid day"))?;
    let month = match parts[2] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return Err(anyhow!("invalid month")),
    };
    let year: i32 = parts[3].parse().map_err(|_| anyhow!("invalid year"))?;
    let hms: Vec<&str> = parts[4].split(':').collect();
    if hms.len() != 3 {
        return Err(anyhow!("invalid time"));
    }
    let hour: u8 = hms[0].parse().map_err(|_| anyhow!("invalid hour"))?;
    let minute: u8 = hms[1].parse().map_err(|_| anyhow!("invalid minute"))?;
    let second: u8 = hms[2].parse().map_err(|_| anyhow!("invalid second"))?;

    let month_enum = ::time::Month::try_from(month).map_err(|_| anyhow!("invalid month num"))?;
    let date = ::time::Date::from_calendar_date(year, month_enum, day)
        .map_err(|e| anyhow!("invalid date: {}", e))?;
    let time = ::time::Time::from_hms(hour, minute, second)
        .map_err(|e| anyhow!("invalid clock: {}", e))?;
    Ok(OffsetDateTime::new_utc(date, time))
}

#[derive(Clone, Debug)]
struct SigningCandidate {
    service: String,
    region: String,
}

fn signing_candidates(config: &RemotelySaveConfig, _host: &str) -> Vec<SigningCandidate> {
    let services: Vec<&str> = vec!["oss"];
    let regions = vec![config.region.clone()];

    let mut out = Vec::new();
    for service in services {
        for region in &regions {
            out.push(SigningCandidate {
                service: service.to_string(),
                region: region.clone(),
            });
        }
    }

    out
}

struct ParsedUrl {
    host: String,
    canonical_uri: String,
    canonical_query: String,
}

fn parse_url(url: &str) -> Result<ParsedUrl> {
    let scheme_sep = url
        .find("://")
        .ok_or_else(|| anyhow!("invalid url: {}", url))?;
    let rest = &url[scheme_sep + 3..];

    let slash_pos = rest.find('/');
    let query_pos = rest.find('?');
    let host_end = match (slash_pos, query_pos) {
        (Some(s), Some(q)) => s.min(q),
        (Some(s), None) => s,
        (None, Some(q)) => q,
        (None, None) => rest.len(),
    };

    let host = rest[..host_end].to_string();
    if host.is_empty() {
        return Err(anyhow!("invalid url host"));
    }

    let tail = &rest[host_end..];
    let (path_raw, query_raw) = if tail.is_empty() {
        ("/", "")
    } else if let Some(qidx) = tail.find('?') {
        let path = if qidx == 0 { "/" } else { &tail[..qidx] };
        (path, &tail[qidx + 1..])
    } else {
        (tail, "")
    };

    let canonical_uri = if path_raw.is_empty() {
        "/".to_string()
    } else {
        path_raw.to_string()
    };
    let canonical_query = canonicalize_query(query_raw);

    Ok(ParsedUrl {
        host,
        canonical_uri,
        canonical_query,
    })
}

fn canonicalize_query(raw_query: &str) -> String {
    if raw_query.is_empty() {
        return String::new();
    }

    let mut pairs = Vec::new();
    for pair in raw_query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        pairs.push((percent_encode(k), percent_encode(v)));
    }

    pairs.sort_by(|a, b| a.cmp(b));
    pairs
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&")
}

struct SigningMaterial {
    request_date: String,
    payload_hash: String,
    authorization: String,
    credential_scope: String,
    canonical_request_sha256: String,
    string_to_sign_sha256: String,
}

fn signing_material(
    config: &RemotelySaveConfig,
    url: &ParsedUrl,
    method: &str,
    candidate: &SigningCandidate,
    timestamp: &SigningTimestamp,
    additional_headers: &[(&str, &str)],
) -> Result<SigningMaterial> {
    let date_stamp = timestamp.date_stamp.clone();
    let request_date = timestamp.request_date.clone();

    // OSS V4 always includes the bucket in the canonical URI path.
    // Virtual-hosted style URLs have the bucket in the host, so we must
    // manually prepend it to the canonical URI for signing.
    let canonical_uri = if !config.force_path_style {
        format!("/{}{}", config.bucket_name, url.canonical_uri)
    } else {
        url.canonical_uri.clone()
    };

    let mut canonical_header_pairs = vec![
        ("host".to_string(), url.host.clone()),
        (
            "x-oss-content-sha256".to_string(),
            EMPTY_PAYLOAD_SHA256.to_string(),
        ),
        ("x-oss-date".to_string(), request_date.clone()),
    ];
    for (name, value) in additional_headers {
        canonical_header_pairs.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    canonical_header_pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut canonical_headers = String::new();
    let mut signed_header_names = Vec::new();
    for (name, value) in &canonical_header_pairs {
        canonical_headers.push_str(name);
        canonical_headers.push(':');
        canonical_headers.push_str(value);
        canonical_headers.push('\n');
        signed_header_names.push(name.as_str());
    }
    let signed_headers = signed_header_names.join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method,
        canonical_uri,
        url.canonical_query,
        canonical_headers,
        signed_headers,
        EMPTY_PAYLOAD_SHA256
    );

    let canonical_request_hash = sha256_hex(canonical_request.as_bytes());
    let credential_scope = format!(
        "{}/{}/{}/aliyun_v4_request",
        date_stamp, candidate.region, candidate.service
    );
    let string_to_sign = format!(
        "OSS4-HMAC-SHA256\n{}\n{}\n{}",
        request_date, credential_scope, canonical_request_hash
    );

    let signing_key = derive_signing_key(
        &config.secret_access_key,
        &date_stamp,
        &candidate.region,
        &candidate.service,
    )?;
    let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes())?);

    let authorization = format!(
        "OSS4-HMAC-SHA256 Credential={}/{}, AdditionalHeaders={}, Signature={}",
        config.access_key_id, credential_scope, signed_headers, signature
    );

    Ok(SigningMaterial {
        request_date,
        payload_hash: EMPTY_PAYLOAD_SHA256.to_string(),
        authorization,
        credential_scope,
        canonical_request_sha256: canonical_request_hash,
        string_to_sign_sha256: sha256_hex(string_to_sign.as_bytes()),
    })
}

fn derive_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Result<Vec<u8>> {
    let k_date = hmac_sha256(format!("aliyun_v4{}", secret).as_bytes(), date.as_bytes())?;
    let k_region = hmac_sha256(&k_date, region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, service.as_bytes())?;
    hmac_sha256(&k_service, b"aliyun_v4_request")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| anyhow!("hmac key error: {}", e))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(hex_lower((b >> 4) & 0x0f));
        out.push(hex_lower(b & 0x0f));
    }
    out
}

fn hex_lower(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        _ => char::from(b'a' + (nibble - 10)),
    }
}

fn key_to_local_path(config: &RemotelySaveConfig, key: &str) -> Result<PathBuf> {
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

    Ok(Path::new(VAULT_ROOT).join(normalized))
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
    if key
        .split('/')
        .any(|part| part == ".obsidian" || part.starts_with('_') || part.starts_with('.'))
    {
        return true;
    }
    false
}

fn read_sync_manifest() -> HashMap<String, SyncManifestEntry> {
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

fn delete_stale_manifest_files(
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

fn write_status_file(
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
