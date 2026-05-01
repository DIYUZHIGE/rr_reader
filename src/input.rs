use anyhow::Result;
use esp_idf_hal::sys;
use log::info;

// ── Hardware button indices ──────────────────────────────────────
pub const BTN_BACK: u8 = 0;
pub const BTN_CONFIRM: u8 = 1;
pub const BTN_LEFT: u8 = 2;
pub const BTN_RIGHT: u8 = 3;
pub const BTN_UP: u8 = 4;
pub const BTN_DOWN: u8 = 5;
pub const BTN_POWER: u8 = 6;
const BUTTON_COUNT: usize = 7;

// ESP32-C3 ADC1 channel mapping:
//   ADC_CHANNEL_1 -> GPIO1 (front button resistor ladder)
//   ADC_CHANNEL_2 -> GPIO2 (side button resistor ladder)
const FRONT_ADC_CHANNEL: sys::adc_channel_t = sys::adc_channel_t_ADC_CHANNEL_1;
const SIDE_ADC_CHANNEL: sys::adc_channel_t = sys::adc_channel_t_ADC_CHANNEL_2;

// ADC ranges from crosspoint measured values
const ADC_NO_BUTTON: u16 = 3800;
const ADC_RANGES_1: [i32; 5] = [ADC_NO_BUTTON as i32, 3100, 2090, 750, i32::MIN];
const ADC_RANGES_2: [i32; 3] = [ADC_NO_BUTTON as i32, 1120, i32::MIN];

const DEBOUNCE_DELAY_MS: u64 = 5;
const ADC_SAMPLE_COUNT: usize = 3;

// ── Logical button types ────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Back,
    Confirm,
    Left,
    Right,
    Up,
    Down,
    Power,
    PageBack,
    PageForward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontButtonRole {
    Back = 0,
    Confirm = 1,
    Left = 2,
    Right = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideButtonLayout {
    PrevNext,
    NextPrev,
}

// ── Input Manager ───────────────────────────────────────────────

pub struct InputManager {
    current_state: u8,
    last_state: u8,
    pressed_events: u8,
    released_events: u8,
    last_debounce_ms: u64,
    button_press_start_ms: [u64; BUTTON_COUNT],
    button_press_finish_ms: [u64; BUTTON_COUNT],
    long_press_consumed: u8,
    front_mapping: [u8; 4],
    side_layout: SideButtonLayout,
}

impl InputManager {
    /// Create the input manager. Peripheral configuration is handled via
    /// raw ESP-IDF calls internally. This avoids lifetime issues with
    /// sharing Peripherals across multiple subsystems.
    pub fn new() -> Result<Self> {
        unsafe {
            // Configure ADC1 channels for button reading
            sys::adc1_config_width(sys::adc_bits_width_t_ADC_WIDTH_BIT_12);
            sys::adc1_config_channel_atten(FRONT_ADC_CHANNEL, sys::adc_atten_t_ADC_ATTEN_DB_11);
            sys::adc1_config_channel_atten(SIDE_ADC_CHANNEL, sys::adc_atten_t_ADC_ATTEN_DB_11);

            // GPIO3: power button (digital input with pull-up)
            sys::gpio_set_direction(sys::gpio_num_t_GPIO_NUM_3, sys::gpio_mode_t_GPIO_MODE_INPUT);
            sys::gpio_set_pull_mode(
                sys::gpio_num_t_GPIO_NUM_3,
                sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY,
            );
        }

        Ok(Self {
            current_state: 0,
            last_state: 0,
            pressed_events: 0,
            released_events: 0,
            last_debounce_ms: 0,
            button_press_start_ms: [0; BUTTON_COUNT],
            button_press_finish_ms: [0; BUTTON_COUNT],
            long_press_consumed: 0,
            front_mapping: [BTN_BACK, BTN_CONFIRM, BTN_LEFT, BTN_RIGHT],
            side_layout: SideButtonLayout::PrevNext,
        })
    }

    pub fn update(&mut self) {
        self.pressed_events = 0;
        self.released_events = 0;

        let now_ms = now_ms();
        let state = self.read_raw_state();

        if state != self.last_state {
            self.last_debounce_ms = now_ms;
            self.last_state = state;
        }

        if now_ms.saturating_sub(self.last_debounce_ms) > DEBOUNCE_DELAY_MS
            && state != self.current_state
        {
            self.pressed_events = state & !self.current_state;
            self.released_events = self.current_state & !state;

            for button in 0..BUTTON_COUNT {
                let mask = 1u8 << button;
                if self.pressed_events & mask != 0 {
                    self.button_press_start_ms[button] = now_ms;
                    self.button_press_finish_ms[button] = 0;
                    self.long_press_consumed &= !mask;
                }
                if self.released_events & mask != 0 {
                    self.button_press_finish_ms[button] = now_ms;
                    self.long_press_consumed &= !mask;
                }
            }

            self.current_state = state;
            info!(
                "Input state: pressed={}, released={}, current=0x{:02x}",
                Self::format_button_mask(self.pressed_events),
                Self::format_button_mask(self.released_events),
                self.current_state
            );
        }
    }

    fn read_raw_state(&self) -> u8 {
        let mut state: u8 = 0;

        unsafe {
            // GPIO1 (front buttons)
            let front_raw = Self::read_adc_median(FRONT_ADC_CHANNEL);
            if let Some(idx) = Self::adc_to_button(front_raw as u16, &ADC_RANGES_1) {
                state |= 1u8 << idx;
            }

            // GPIO2 (side buttons)
            let side_raw = Self::read_adc_median(SIDE_ADC_CHANNEL);
            if let Some(idx) = Self::adc_to_button(side_raw as u16, &ADC_RANGES_2) {
                state |= 1u8 << (idx + 4); // BTN_UP=4, BTN_DOWN=5
            }

            // GPIO3 (power button, active LOW)
            if sys::gpio_get_level(sys::gpio_num_t_GPIO_NUM_3) == 0 {
                state |= 1u8 << BTN_POWER;
            }
        }

        state
    }

    unsafe fn read_adc_median(channel: sys::adc_channel_t) -> i32 {
        let mut samples = [0i32; ADC_SAMPLE_COUNT];
        for sample in &mut samples {
            *sample = sys::adc1_get_raw(channel);
        }

        if samples[0] > samples[1] {
            samples.swap(0, 1);
        }
        if samples[1] > samples[2] {
            samples.swap(1, 2);
        }
        if samples[0] > samples[1] {
            samples.swap(0, 1);
        }
        samples[1]
    }

    fn adc_to_button(adc_value: u16, ranges: &[i32]) -> Option<u8> {
        let v = adc_value as i32;
        for i in 0..(ranges.len() - 1) {
            if v > ranges[i + 1] && v <= ranges[i] {
                return Some(i as u8);
            }
        }
        None
    }

    fn format_button_mask(mask: u8) -> &'static str {
        match mask {
            0 => "-",
            m if m == (1u8 << BTN_BACK) => "Back",
            m if m == (1u8 << BTN_CONFIRM) => "Confirm",
            m if m == (1u8 << BTN_LEFT) => "Left",
            m if m == (1u8 << BTN_RIGHT) => "Right",
            m if m == (1u8 << BTN_UP) => "Up",
            m if m == (1u8 << BTN_DOWN) => "Down",
            m if m == (1u8 << BTN_POWER) => "Power",
            _ => "Multiple",
        }
    }

    // ── Queries ──────────────────────────────────────────────────

    pub fn is_pressed(&self, button_index: u8) -> bool {
        self.current_state & (1u8 << button_index) != 0
    }

    pub fn was_pressed(&self, button_index: u8) -> bool {
        self.pressed_events & (1u8 << button_index) != 0
    }

    pub fn was_released(&self, button_index: u8) -> bool {
        self.released_events & (1u8 << button_index) != 0
    }

    pub fn has_user_activity(&self) -> bool {
        self.current_state != 0 || self.pressed_events != 0 || self.released_events != 0
    }

    pub fn was_any_pressed(&self) -> bool {
        self.pressed_events != 0
    }

    pub fn was_any_released(&self) -> bool {
        self.released_events != 0
    }

    pub fn any_pressed(&self) -> bool {
        self.current_state != 0
    }

    pub fn held_ms_any(&self) -> u32 {
        (0..BUTTON_COUNT)
            .map(|button| self.held_ms(button as u8))
            .max()
            .unwrap_or(0)
    }

    pub fn held_ms(&self, button_index: u8) -> u32 {
        let button = button_index as usize;
        if button >= BUTTON_COUNT {
            return 0;
        }

        let start = self.button_press_start_ms[button];
        if start == 0 {
            return 0;
        }

        let end = if self.is_pressed(button_index) {
            now_ms()
        } else {
            self.button_press_finish_ms[button]
        };

        end.saturating_sub(start).min(u32::MAX as u64) as u32
    }

    pub fn long_pressed_once(&mut self, button_index: u8, threshold_ms: u32) -> bool {
        let button = button_index as usize;
        if button >= BUTTON_COUNT || !self.is_pressed(button_index) {
            return false;
        }

        let mask = 1u8 << button_index;
        if self.long_press_consumed & mask != 0 || self.held_ms(button_index) < threshold_ms {
            return false;
        }

        self.long_press_consumed |= mask;
        true
    }

    pub fn clear_events(&mut self) {
        self.pressed_events = 0;
        self.released_events = 0;
    }

    // ── Logical mapping ──────────────────────────────────────────

    pub fn logical_to_physical(&self, button: Button) -> u8 {
        match button {
            Button::Back => self.front_mapping[FrontButtonRole::Back as usize],
            Button::Confirm => self.front_mapping[FrontButtonRole::Confirm as usize],
            Button::Left => self.front_mapping[FrontButtonRole::Left as usize],
            Button::Right => self.front_mapping[FrontButtonRole::Right as usize],
            Button::Up => BTN_UP,
            Button::Down => BTN_DOWN,
            Button::Power => BTN_POWER,
            Button::PageBack => match self.side_layout {
                SideButtonLayout::PrevNext => BTN_UP,
                SideButtonLayout::NextPrev => BTN_DOWN,
            },
            Button::PageForward => match self.side_layout {
                SideButtonLayout::PrevNext => BTN_DOWN,
                SideButtonLayout::NextPrev => BTN_UP,
            },
        }
    }

    pub fn logical_is_pressed(&self, button: Button) -> bool {
        self.is_pressed(self.logical_to_physical(button))
    }

    pub fn logical_was_pressed(&self, button: Button) -> bool {
        self.was_pressed(self.logical_to_physical(button))
    }

    pub fn logical_was_released(&self, button: Button) -> bool {
        self.was_released(self.logical_to_physical(button))
    }

    pub fn logical_held_ms(&self, button: Button) -> u32 {
        self.held_ms(self.logical_to_physical(button))
    }

    pub fn any_logical_pressed(&self, buttons: &[Button]) -> bool {
        buttons.iter().any(|&b| self.logical_was_pressed(b))
    }

    pub fn any_logical_released(&self, buttons: &[Button]) -> bool {
        buttons.iter().any(|&b| self.logical_was_released(b))
    }

    pub fn any_logical_is_pressed(&self, buttons: &[Button]) -> bool {
        buttons.iter().any(|&b| self.logical_is_pressed(b))
    }

    pub fn any_logical_held_ms(&self, buttons: &[Button]) -> u32 {
        buttons
            .iter()
            .map(|&button| self.logical_held_ms(button))
            .max()
            .unwrap_or(0)
    }

    pub fn set_front_button(&mut self, role: FrontButtonRole, hw_index: u8) {
        self.front_mapping[role as usize] = hw_index;
    }

    pub fn set_side_layout(&mut self, layout: SideButtonLayout) {
        self.side_layout = layout;
    }
}

fn now_ms() -> u64 {
    unsafe { (sys::esp_timer_get_time() / 1000) as u64 }
}
