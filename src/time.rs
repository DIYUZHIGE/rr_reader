use esp_idf_hal::sys;

pub fn now_ms() -> u64 {
    unsafe { (sys::esp_timer_get_time() / 1000) as u64 }
}
