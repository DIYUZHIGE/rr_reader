use super::SD_MOUNT_POINT;
use anyhow::{anyhow, Result};
use log::info;
use std::fs;
use std::path::Path;

const WIFI_CONFIG_PATHS: [&str; 3] = [
    "/sdcard/wifi.conf",
    "/sdcard/wifi.txt",
    "/sdcard/vault/wifi.conf",
];

#[derive(Clone, Debug)]
pub struct WifiCredentials {
    pub ssid: String,
    pub password: String,
    pub source_path: String,
}

impl super::Storage {
    pub fn read_wifi_credentials(&self) -> Result<Option<WifiCredentials>> {
        for path in WIFI_CONFIG_PATHS {
            if !Path::new(path).exists() {
                continue;
            }

            let contents = fs::read_to_string(path).map_err(|e| anyhow!("read {}: {}", path, e))?;
            let mut credentials =
                parse_wifi_credentials(&contents).map_err(|e| anyhow!("parse {}: {}", path, e))?;
            credentials.source_path = path.to_owned();
            info!("Loaded WiFi credentials from {}", path);
            return Ok(Some(credentials));
        }

        info!("No WiFi config found under {}", SD_MOUNT_POINT);
        Ok(None)
    }
}

fn parse_wifi_credentials(contents: &str) -> Result<WifiCredentials> {
    let mut ssid = None;
    let mut password = None;
    let mut positional: [Option<String>; 2] = [None, None];
    let mut positional_count = 0;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            let value = unquote(value.trim());
            match key.trim().to_ascii_lowercase().as_str() {
                "ssid" | "wifi_ssid" => ssid = Some(value),
                "password" | "pass" | "wifi_pass" | "wifi_password" => password = Some(value),
                _ => {}
            }
        } else if positional_count < 2 {
            positional[positional_count] = Some(unquote(line));
            positional_count += 1;
        }
    }

    if ssid.is_none() {
        ssid = positional[0].take();
    }
    if password.is_none() {
        password = positional[1].take();
    }

    let ssid = ssid.ok_or_else(|| anyhow!("missing ssid"))?;
    let password = password.unwrap_or_default();

    validate_wifi_field("ssid", &ssid, 1, 32)?;
    validate_wifi_field("password", &password, 0, 64)?;

    Ok(WifiCredentials {
        ssid,
        password,
        source_path: String::new(),
    })
}

fn validate_wifi_field(name: &str, value: &str, min_len: usize, max_len: usize) -> Result<()> {
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
