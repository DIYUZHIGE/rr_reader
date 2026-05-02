mod path;
mod sdcard;
mod vault;
mod wifi;

pub use self::sdcard::Storage;
pub use self::wifi::WifiCredentials;

pub(super) const SD_MOUNT_POINT: &str = "/sdcard";
pub(super) const VAULT_DIR: &str = "vault";
