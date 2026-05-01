use anyhow::Result;
use esp_idf_hal::sys;

// ── Hardware button indices ──────────────────────────────────────
pub const BTN_BACK: u8 = 0;
pub const BTN_CONFIRM: u8 = 1;
pub const BTN_LEFT: u8 = 2;
pub const BTN_RIGHT: u8 = 3;
pub const BTN_UP: u8 = 4;
pub const BTN_DOWN: u8 = 5;
pub const BTN_POWER: u8 = 6;
const BUTTON_COUNT: usize = 7;

// ADC ranges from crosspoint measured values
const ADC_NO_BUTTON: u16 = 3800;
const ADC_RANGES_1: [i32; 5] = [ADC_NO_BUTTON as i32, 3100, 2090, 750, i32::MIN];
const ADC_RANGES_2: [i32; 3] = [ADC_NO_BUTTON as i32, 1120, i32::MIN];

const DEBOUNCE_TICKS: u8 = 2;

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
    last_state: u8,
    debounced_state: u8,
    debounce_counter: u8,
    pressed_events: u8,
    released_events: u8,
    hold_ticks: [u32; BUTTON_COUNT],
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
            sys::adc1_config_channel_atten(
                sys::adc_channel_t_ADC_CHANNEL_0,
                sys::adc_atten_t_ADC_ATTEN_DB_11,
            );
            sys::adc1_config_channel_atten(
                sys::adc_channel_t_ADC_CHANNEL_1,
                sys::adc_atten_t_ADC_ATTEN_DB_11,
            );

            // GPIO3: power button (digital input with pull-up)
            sys::gpio_set_direction(sys::gpio_num_t_GPIO_NUM_3, sys::gpio_mode_t_GPIO_MODE_INPUT);
            sys::gpio_set_pull_mode(
                sys::gpio_num_t_GPIO_NUM_3,
                sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY,
            );
        }

        Ok(Self {
            last_state: 0,
            debounced_state: 0,
            debounce_counter: 0,
            pressed_events: 0,
            released_events: 0,
            hold_ticks: [0; BUTTON_COUNT],
            front_mapping: [BTN_BACK, BTN_CONFIRM, BTN_LEFT, BTN_RIGHT],
            side_layout: SideButtonLayout::PrevNext,
        })
    }

    pub fn update(&mut self) {
        self.pressed_events = 0;
        self.released_events = 0;

        let raw_state = self.read_raw_state();

        if raw_state != self.last_state {
            self.debounce_counter = 0;
            self.last_state = raw_state;
        } else if self.debounce_counter < DEBOUNCE_TICKS {
            self.debounce_counter = self.debounce_counter.saturating_add(1);
        }

        if self.debounce_counter >= DEBOUNCE_TICKS && raw_state != self.debounced_state {
            self.pressed_events = raw_state & !self.debounced_state;
            self.released_events = !raw_state & self.debounced_state;
            self.debounced_state = raw_state;

            for i in 0..BUTTON_COUNT {
                if self.pressed_events & (1u8 << i) != 0 {
                    self.hold_ticks[i] = 0;
                }
            }
        }

        for i in 0..BUTTON_COUNT {
            if self.debounced_state & (1u8 << i) != 0 {
                self.hold_ticks[i] = self.hold_ticks[i].saturating_add(1);
            } else {
                self.hold_ticks[i] = 0;
            }
        }
    }

    fn read_raw_state(&self) -> u8 {
        let mut state: u8 = 0;

        unsafe {
            // GPIO1 (front buttons) — ADC1_CH0
            let front_raw = sys::adc1_get_raw(sys::adc_channel_t_ADC_CHANNEL_0);
            if let Some(idx) = Self::adc_to_button(front_raw as u16, &ADC_RANGES_1) {
                state |= 1u8 << idx;
            }

            // GPIO2 (side buttons) — ADC1_CH1
            let side_raw = sys::adc1_get_raw(sys::adc_channel_t_ADC_CHANNEL_1);
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

    fn adc_to_button(adc_value: u16, ranges: &[i32]) -> Option<u8> {
        let v = adc_value as i32;
        for i in 0..(ranges.len() - 1) {
            if v > ranges[i + 1] && v <= ranges[i] {
                return Some(i as u8);
            }
        }
        None
    }

    // ── Queries ──────────────────────────────────────────────────

    pub fn is_pressed(&self, button_index: u8) -> bool {
        self.debounced_state & (1u8 << button_index) != 0
    }

    pub fn was_pressed(&self, button_index: u8) -> bool {
        self.pressed_events & (1u8 << button_index) != 0
    }

    pub fn was_released(&self, button_index: u8) -> bool {
        self.released_events & (1u8 << button_index) != 0
    }

    pub fn has_user_activity(&self) -> bool {
        self.pressed_events != 0 || self.released_events != 0
    }

    pub fn any_pressed(&self) -> bool {
        self.debounced_state != 0
    }

    pub fn held_ms(&self, button_index: u8) -> u32 {
        self.hold_ticks[button_index as usize] * 10
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

    pub fn any_logical_pressed(&self, buttons: &[Button]) -> bool {
        buttons.iter().any(|&b| self.logical_was_pressed(b))
    }

    pub fn any_logical_released(&self, buttons: &[Button]) -> bool {
        buttons.iter().any(|&b| self.logical_was_released(b))
    }

    pub fn set_front_button(&mut self, role: FrontButtonRole, hw_index: u8) {
        self.front_mapping[role as usize] = hw_index;
    }

    pub fn set_side_layout(&mut self, layout: SideButtonLayout) {
        self.side_layout = layout;
    }
}
