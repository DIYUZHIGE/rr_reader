use esp_idf_hal::sys;

pub const POWER_BUTTON_GPIO: sys::gpio_num_t = sys::gpio_num_t_GPIO_NUM_3;
pub const FRONT_ADC_CHANNEL: sys::adc_channel_t = sys::adc_channel_t_ADC_CHANNEL_1;
pub const SIDE_ADC_CHANNEL: sys::adc_channel_t = sys::adc_channel_t_ADC_CHANNEL_2;
pub const INPUT_ADC_WIDTH: sys::adc_bits_width_t = sys::adc_bits_width_t_ADC_WIDTH_BIT_12;
pub const INPUT_ADC_ATTEN: sys::adc_atten_t = sys::adc_atten_t_ADC_ATTEN_DB_11;
pub const SPI_SCLK_GPIO: i32 = 8;
pub const SPI_MISO_GPIO: i32 = 7;
pub const SPI_MOSI_GPIO: i32 = 10;
