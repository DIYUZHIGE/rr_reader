use crate::storage::{Storage, WifiCredentials};
use anyhow::{anyhow, Result};
use core::convert::TryInto;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi,
};
use log::{info, warn};

#[derive(Clone, Debug)]
pub enum WifiStatus {
    NotConfigured,
    Connected { ssid: String, ip: String },
    Failed { ssid: Option<String>, reason: String },
}

impl WifiStatus {
    pub fn boot_line(&self) -> String {
        match self {
            Self::NotConfigured => "WiFi: 未配置".to_owned(),
            Self::Connected { ssid, ip } => format!("WiFi: {} {}", ssid, ip),
            Self::Failed { ssid, reason } => match ssid {
                Some(ssid) => format!("WiFi: {} 失败: {}", ssid, reason),
                None => format!("WiFi: 失败: {}", reason),
            },
        }
    }
}

pub struct NetworkManager {
    modem: Option<Modem<'static>>,
    wifi: Option<BlockingWifi<EspWifi<'static>>>,
    status: WifiStatus,
}

impl NetworkManager {
    pub fn new(modem: Modem<'static>) -> Self {
        Self {
            modem: Some(modem),
            wifi: None,
            status: WifiStatus::NotConfigured,
        }
    }

    pub fn status(&self) -> &WifiStatus {
        &self.status
    }

    pub fn connect_from_storage(&mut self, storage: &Storage) -> WifiStatus {
        let credentials = match storage.read_wifi_credentials() {
            Ok(Some(credentials)) => credentials,
            Ok(None) => {
                self.status = WifiStatus::NotConfigured;
                return self.status.clone();
            }
            Err(e) => {
                warn!("WiFi config error: {}", e);
                self.status = WifiStatus::Failed {
                    ssid: None,
                    reason: e.to_string(),
                };
                return self.status.clone();
            }
        };

        match self.connect(credentials) {
            Ok(status) => {
                self.status = status;
            }
            Err(e) => {
                warn!("WiFi connection failed: {}", e);
                self.status = WifiStatus::Failed {
                    ssid: None,
                    reason: e.to_string(),
                };
            }
        }

        self.status.clone()
    }

    fn connect(&mut self, credentials: WifiCredentials) -> Result<WifiStatus> {
        let ssid = credentials.ssid.clone();
        let password_len = credentials.password.as_bytes().len();
        info!(
            "Connecting WiFi ssid={} password_len={} from {}",
            ssid, password_len, credentials.source_path
        );

        if self.wifi.is_none() {
            let modem = self
                .modem
                .take()
                .ok_or_else(|| anyhow!("WiFi modem is already in use"))?;
            let sys_loop = EspSystemEventLoop::take()?;
            let nvs = EspDefaultNvsPartition::take()?;
            let wifi = EspWifi::new(modem, sys_loop.clone(), Some(nvs))?;
            self.wifi = Some(BlockingWifi::wrap(wifi, sys_loop)?);
        }

        let wifi = self
            .wifi
            .as_mut()
            .ok_or_else(|| anyhow!("WiFi driver is unavailable"))?;

        let configuration = Configuration::Client(ClientConfiguration {
            ssid: credentials
                .ssid
                .as_str()
                .try_into()
                .map_err(|_| anyhow!("ssid is too long"))?,
            bssid: None,
            auth_method: if credentials.password.is_empty() {
                AuthMethod::None
            } else {
                AuthMethod::WPA2Personal
            },
            password: credentials
                .password
                .as_str()
                .try_into()
                .map_err(|_| anyhow!("password is too long"))?,
            channel: None,
            ..Default::default()
        });

        wifi.set_configuration(&configuration)?;
        wifi.start()?;
        wifi.connect().map_err(|e| anyhow!("connect {}: {}", ssid, e))?;
        wifi.wait_netif_up()
            .map_err(|e| anyhow!("wait DHCP {}: {}", ssid, e))?;

        let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
        let ip = ip_info.ip.to_string();
        info!("WiFi connected: ssid={} ip={}", ssid, ip);

        Ok(WifiStatus::Connected { ssid, ip })
    }
}

