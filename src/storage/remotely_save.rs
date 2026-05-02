use super::SD_MOUNT_POINT;
use anyhow::{anyhow, Result};
use log::info;
use serde_json::Value;
use std::fs;
use std::path::Path;

const REMOTELY_SAVE_CONFIG_PATHS: [&str; 3] = [
    "/sdcard/remotely_save.conf",
    "/sdcard/remotely_save.txt",
    "/sdcard/vault/remotely_save.conf",
];

#[derive(Clone, Debug)]
pub struct RemotelySaveConfig {
    pub endpoint: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket_name: String,
    pub remote_prefix: String,
    pub force_path_style: bool,
    pub source_path: String,
}

impl super::Storage {
    pub fn read_remotely_save_config(&self) -> Result<Option<RemotelySaveConfig>> {
        for path in REMOTELY_SAVE_CONFIG_PATHS {
            if !Path::new(path).exists() {
                continue;
            }

            let contents = fs::read_to_string(path).map_err(|e| anyhow!("read {}: {}", path, e))?;
            let mut config = parse_remotely_save_config(&contents)
                .map_err(|e| anyhow!("parse {}: {}", path, e))?;
            config.source_path = path.to_owned();
            info!("Loaded remotely-save config from {}", path);
            return Ok(Some(config));
        }

        info!("No remotely-save config found under {}", SD_MOUNT_POINT);
        Ok(None)
    }
}

fn parse_remotely_save_config(contents: &str) -> Result<RemotelySaveConfig> {
    let trimmed = contents.trim();
    if trimmed.starts_with("obsidian://remotely-save?") {
        return parse_from_deep_link(trimmed);
    }

    parse_from_key_values(trimmed)
}

fn parse_from_deep_link(link: &str) -> Result<RemotelySaveConfig> {
    let query = link
        .split_once('?')
        .map(|(_, q)| q)
        .ok_or_else(|| anyhow!("missing query in deep link"))?;

    let mut data_param = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "data" {
            data_param = Some(value);
            break;
        }
    }

    let data = data_param.ok_or_else(|| anyhow!("missing data query parameter"))?;
    let decoded = percent_decode(data)?;

    let payload: Value =
        serde_json::from_str(&decoded).map_err(|e| anyhow!("invalid data JSON: {}", e))?;
    let s3 = payload
        .get("s3")
        .ok_or_else(|| anyhow!("missing s3 object in data JSON"))?;

    let endpoint = read_json_string(s3, "s3Endpoint")?;
    let region = read_json_string(s3, "s3Region")?;
    let access_key_id = read_json_string(s3, "s3AccessKeyID")?;
    let secret_access_key = read_json_string(s3, "s3SecretAccessKey")?;
    let bucket_name = read_json_string(s3, "s3BucketName")?;
    let remote_prefix = read_json_string_optional(s3, "remotePrefix").unwrap_or_default();
    let force_path_style = read_json_bool_optional(s3, "forcePathStyle").unwrap_or(false);

    validate_len("s3Endpoint", &endpoint, 1, 512)?;
    validate_len("s3Region", &region, 1, 128)?;
    validate_len("s3AccessKeyID", &access_key_id, 1, 128)?;
    validate_len("s3SecretAccessKey", &secret_access_key, 1, 256)?;
    validate_len("s3BucketName", &bucket_name, 1, 128)?;

    Ok(RemotelySaveConfig {
        endpoint,
        region,
        access_key_id,
        secret_access_key,
        bucket_name,
        remote_prefix,
        force_path_style,
        source_path: String::new(),
    })
}

fn parse_from_key_values(contents: &str) -> Result<RemotelySaveConfig> {
    let mut endpoint = None;
    let mut region = None;
    let mut access_key_id = None;
    let mut secret_access_key = None;
    let mut bucket_name = None;
    let mut remote_prefix = String::new();
    let mut force_path_style = false;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }

        let Some((key, value_raw)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim().to_ascii_lowercase();
        let value = unquote(value_raw.trim());
        match key.as_str() {
            "endpoint" | "s3_endpoint" => endpoint = Some(value),
            "region" | "s3_region" => region = Some(value),
            "access_key_id" | "s3_access_key_id" => access_key_id = Some(value),
            "secret_access_key" | "s3_secret_access_key" => secret_access_key = Some(value),
            "bucket" | "bucket_name" | "s3_bucket_name" => bucket_name = Some(value),
            "remote_prefix" | "prefix" => remote_prefix = value,
            "force_path_style" => {
                force_path_style = match value.to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => true,
                    "0" | "false" | "no" | "off" => false,
                    _ => return Err(anyhow!("invalid force_path_style: {}", value)),
                }
            }
            _ => {}
        }
    }

    let endpoint = endpoint.ok_or_else(|| anyhow!("missing endpoint"))?;
    let region = region.ok_or_else(|| anyhow!("missing region"))?;
    let access_key_id = access_key_id.ok_or_else(|| anyhow!("missing access_key_id"))?;
    let secret_access_key =
        secret_access_key.ok_or_else(|| anyhow!("missing secret_access_key"))?;
    let bucket_name = bucket_name.ok_or_else(|| anyhow!("missing bucket_name"))?;

    validate_len("endpoint", &endpoint, 1, 512)?;
    validate_len("region", &region, 1, 128)?;
    validate_len("access_key_id", &access_key_id, 1, 128)?;
    validate_len("secret_access_key", &secret_access_key, 1, 256)?;
    validate_len("bucket_name", &bucket_name, 1, 128)?;

    Ok(RemotelySaveConfig {
        endpoint,
        region,
        access_key_id,
        secret_access_key,
        bucket_name,
        remote_prefix,
        force_path_style,
        source_path: String::new(),
    })
}

fn read_json_string(object: &Value, key: &str) -> Result<String> {
    object
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| anyhow!("missing {} in data JSON", key))
}

fn read_json_string_optional(object: &Value, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

fn read_json_bool_optional(object: &Value, key: &str) -> Option<bool> {
    object.get(key).and_then(|v| v.as_bool())
}

fn validate_len(name: &str, value: &str, min_len: usize, max_len: usize) -> Result<()> {
    let len = value.as_bytes().len();
    if len < min_len || len > max_len {
        return Err(anyhow!(
            "{} must be {}..={} bytes, got {}",
            name,
            min_len,
            max_len,
            len
        ));
    }
    Ok(())
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_owned();
        }
    }

    value.to_owned()
}

fn percent_decode(input: &str) -> Result<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(anyhow!("incomplete percent-encoding"));
            }
            let high = hex_value(bytes[i + 1]).ok_or_else(|| anyhow!("invalid hex digit"))?;
            let low = hex_value(bytes[i + 2]).ok_or_else(|| anyhow!("invalid hex digit"))?;
            out.push(high * 16 + low);
            i += 3;
            continue;
        }

        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }

    String::from_utf8(out).map_err(|e| anyhow!("decoded utf8 error: {}", e))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
