mod path;
mod remotely_save;
mod sdcard;
mod vault;
mod wifi;

pub use self::remotely_save::RemotelySaveConfig;
pub use self::sdcard::Storage;
pub use self::wifi::WifiCredentials;

pub(super) const SD_MOUNT_POINT: &str = "/sdcard";
pub(super) const VAULT_DIR: &str = "vault";
