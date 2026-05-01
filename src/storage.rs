mod path;
mod sdcard;
mod vault;

pub use self::sdcard::Storage;

pub(super) const SD_MOUNT_POINT: &str = "/sdcard";
pub(super) const VAULT_DIR: &str = "vault";
