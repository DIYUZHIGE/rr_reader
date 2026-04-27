use anyhow::Result;
use esp_idf_hal::reset::WakeupReason;
use log::{debug, info};

pub fn handle_wakeup(reason: WakeupReason) -> Result<()> {
    match reason {
        WakeupReason::Unknown => debug!("Wakeup reason: unknown or cold boot"),
        other => info!("Wakeup reason: {:?}", other),
    }

    Ok(())
}
