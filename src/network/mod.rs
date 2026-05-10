use crate::storage::{Storage, WifiCredentials};
use anyhow::{anyhow, Result};
use core::convert::TryInto;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
pub use esp_idf_svc::wifi::AccessPointInfo;
use esp_idf_svc::wifi::{
    AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi,
};
use log::{info, warn};

fn free_heap() -> u32 {
    unsafe { esp_idf_hal::sys::esp_get_free_heap_size() }
}

#[derive(Clone, Debug)]
pub enum WifiStatus {
    NotConfigured,
    Connected {
        ssid: String,
        ip: String,
    },
    Failed {
        ssid: Option<String>,
        reason: String,
    },
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
    last_credentials: Option<WifiCredentials>,
    suspended: bool,
}

impl NetworkManager {
    pub fn new(modem: Modem<'static>) -> Self {
        Self {
            modem: Some(modem),
            wifi: None,
            status: WifiStatus::NotConfigured,
            last_credentials: None,
            suspended: false,
        }
    }

    #[allow(dead_code)]
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

        match self.connect(credentials.clone()) {
            Ok(status) => {
                self.last_credentials = Some(credentials);
                self.suspended = false;
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

    pub fn suspend(&mut self) {
        if let Some(wifi) = self.wifi.as_mut() {
            if let Err(e) = wifi.disconnect() {
                warn!("WiFi disconnect failed: {}", e);
            }
            if let Err(e) = wifi.stop() {
                warn!("WiFi stop failed: {}", e);
            }
            self.suspended = true;
            info!("WiFi suspended for reader mode");
        }
    }

    pub fn shutdown_after_sync(&mut self) {
        if let Some(wifi) = self.wifi.as_mut() {
            warn!("Heap before WiFi shutdown: {}", free_heap());
            if let Err(e) = wifi.disconnect() {
                warn!("WiFi disconnect after sync failed: {}", e);
            }
            if let Err(e) = wifi.stop() {
                warn!("WiFi stop after sync failed: {}", e);
            }
        }
        self.suspended = false;
        warn!("WiFi stopped after sync heap={}", free_heap());
    }

    pub fn resume(&mut self) {
        if !self.suspended {
            return;
        }
        let Some(credentials) = self.last_credentials.clone() else {
            return;
        };
        match self.connect(credentials) {
            Ok(status) => {
                self.suspended = false;
                self.status = status;
                info!("WiFi resumed after reader mode");
            }
            Err(e) => {
                warn!("WiFi resume failed: {}", e);
                self.status = WifiStatus::Failed {
                    ssid: None,
                    reason: e.to_string(),
                };
            }
        }
    }

    /// Scan for nearby WiFi access points. Initializes the WiFi driver in station
    /// mode if not already running. Results are sorted by signal strength (best first).
    pub fn scan(&mut self) -> Result<Vec<AccessPointInfo>> {
        self.ensure_wifi_started()?;
        let wifi = self
            .wifi
            .as_mut()
            .ok_or_else(|| anyhow!("WiFi driver unavailable"))?;

        info!("Starting WiFi scan...");
        let mut aps = wifi
            .scan()
            .map_err(|e| anyhow!("WiFi scan failed: {}", e))?;
        aps.sort_by(|a, b| b.signal_strength.cmp(&a.signal_strength));
        info!("WiFi scan complete: {} access points found", aps.len());
        Ok(aps)
    }

    /// Connect using explicitly provided credentials (not from storage).
    pub fn connect_with_credentials(&mut self, ssid: &str, password: &str) -> WifiStatus {
        let credentials = WifiCredentials {
            ssid: ssid.to_string(),
            password: password.to_string(),
            source_path: "(manual)".to_string(),
        };

        match self.connect(credentials.clone()) {
            Ok(status) => {
                self.last_credentials = Some(credentials);
                self.suspended = false;
                self.status = status;
            }
            Err(e) => {
                warn!("Manual WiFi connection failed: {}", e);
                self.status = WifiStatus::Failed {
                    ssid: Some(ssid.to_string()),
                    reason: e.to_string(),
                };
            }
        }

        self.status.clone()
    }

    /// Disconnect and stop WiFi. Used when leaving WiFi settings to free heap.
    /// Unlike `shutdown_after_sync`, this does not log heap warnings.
    pub fn disconnect_wifi(&mut self) {
        if let Some(wifi) = self.wifi.as_mut() {
            let _ = wifi.disconnect();
            let _ = wifi.stop();
        }
        self.suspended = false;
        info!("WiFi disconnected");
    }

    /// Ensure WiFi driver is initialized and started in station mode.
    fn ensure_wifi_started(&mut self) -> Result<()> {
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
            .ok_or_else(|| anyhow!("WiFi driver unavailable"))?;

        let configuration = Configuration::Client(ClientConfiguration::default());
        wifi.set_configuration(&configuration)?;
        wifi.start()?;
        Ok(())
    }

    fn connect(&mut self, credentials: WifiCredentials) -> Result<WifiStatus> {
        warn!("Heap before WiFi connect: {}", free_heap());
        let ssid = credentials.ssid.clone();
        let password_len = credentials.password.as_bytes().len();
        info!(
            "Connecting WiFi ssid={} password_len={} from {}",
            ssid, password_len, credentials.source_path
        );

        self.ensure_wifi_started()?;

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
        wifi.connect()
            .map_err(|e| anyhow!("connect {}: {}", ssid, e))?;
        wifi.wait_netif_up()
            .map_err(|e| anyhow!("wait DHCP {}: {}", ssid, e))?;

        let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
        let ip = ip_info.ip.to_string();
        warn!(
            "WiFi connected: ssid={} ip={} heap={}",
            ssid,
            ip,
            free_heap()
        );

        Ok(WifiStatus::Connected { ssid, ip })
    }
}
