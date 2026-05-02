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
use std::fs;
use std::path::{Component, Path, PathBuf};

type HmacSha256 = Hmac<Sha256>;

const VAULT_ROOT: &str = "/sdcard/vault";
const SYNC_STATUS_PATH: &str = "/sdcard/vault/.rr_sync_status";
const EMPTY_PAYLOAD_SHA256: &str = "UNSIGNED-PAYLOAD";

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
    ensure_time_synced()?;

    let list_url = build_list_url(config)?;
    info!("Listing remote objects: {}", list_url);
    let list_xml = http_get_text_signed(config, &list_url)?;
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
        match http_get_bytes_signed(config, &object_url) {
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

fn ensure_time_synced() -> Result<()> {
    let now_before = OffsetDateTime::now_utc();
    if now_before.year() >= 2024 {
        return Ok(());
    }

    let sntp = EspSntp::new_default().map_err(|e| anyhow!("SNTP init failed: {}", e))?;

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

fn build_list_url(config: &RemotelySaveConfig) -> Result<String> {
    let base = object_base_url(config)?;
    let mut url = format!("{}/?list-type=2", base);
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

fn http_get_text_signed(config: &RemotelySaveConfig, url: &str) -> Result<String> {
    let bytes = http_get_bytes_signed(config, url)?;
    String::from_utf8(bytes).map_err(|e| anyhow!("utf8 decode {}: {}", url, e))
}

fn http_get_bytes_signed(config: &RemotelySaveConfig, url: &str) -> Result<Vec<u8>> {
    let parsed = parse_url(url)?;
    let candidates = signing_candidates(config, &parsed.host);
    let timestamp = resolve_signing_timestamp(url)?;

    let mut last_error: Option<anyhow::Error> = None;
    for candidate in candidates {
        match http_get_bytes_signed_once(config, url, &parsed, &candidate, &timestamp) {
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

fn http_get_bytes_signed_once(
    config: &RemotelySaveConfig,
    url: &str,
    parsed: &ParsedUrl,
    candidate: &SigningCandidate,
    timestamp: &SigningTimestamp,
) -> Result<Vec<u8>> {
    let signing = signing_material(config, parsed, "GET", candidate, timestamp)?;

    let config_http = HttpConfiguration {
        buffer_size: Some(4096),
        buffer_size_tx: Some(2048),
        timeout: Some(core::time::Duration::from_secs(30)),
        use_global_ca_store: true,
        crt_bundle_attach: Some(attach_crt_bundle),
        ..Default::default()
    };
    let mut connection = EspHttpConnection::new(&config_http)?;

    let auth_header = signing.authorization;
    let request_date = signing.request_date;
    let payload_hash = signing.payload_hash;

    let headers = [
        ("Host", parsed.host.as_str()),
        ("x-oss-date", request_date.as_str()),
        ("x-oss-content-sha256", payload_hash.as_str()),
        ("Authorization", auth_header.as_str()),
    ];

    connection.initiate_request(Method::Get, url, &headers)?;
    connection.initiate_response()?;

    let status = connection.status();
    let body = read_http_body(&mut connection, url)?;

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

fn read_http_body(connection: &mut EspHttpConnection, url: &str) -> Result<Vec<u8>> {
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

    if let Some(server_dt) = fetch_server_datetime(url)? {
        return Ok(signing_timestamp_from_datetime(server_dt));
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
    connection.initiate_request(Method::Get, url, &headers)?;
    connection.initiate_response()?;

    let server_date = connection.header("Date").map(|s| s.to_string());
    let _ = read_http_body(&mut connection, url);

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

    let canonical_headers = format!(
        "host:{}\nx-oss-content-sha256:{}\nx-oss-date:{}\n",
        url.host, EMPTY_PAYLOAD_SHA256, request_date
    );
    let signed_headers = "host;x-oss-content-sha256;x-oss-date";

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
