use crate::hardware::Hardware;
use crate::input::{Button, InputManager, BTN_POWER};
use crate::power::{PowerManager, POWER_BUTTON_SLEEP_MS};
use crate::time::now_ms;

const INPUT_STARTUP_IGNORE_MS: u64 = 300;
const BROWSER_REPEAT_START_MS: u32 = 280;
const BROWSER_REPEAT_INTERVAL_MS: u64 = 140;
const READER_REPEAT_START_MS: u32 = 500;
const READER_REPEAT_INTERVAL_MS: u64 = 450;

#[derive(Clone, Copy, Debug)]
pub(super) enum EventMode {
    FileBrowser,
    Reader,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum AppEvent {
    PowerLongPress,
    BrowserMove(isize),
    BrowserConfirm,
    ReaderBack,
    ReaderMove(isize),
    ReaderRefresh,
    IdleTimeout { idle_ticks: u32 },
}

pub(super) struct EventPump {
    input_ignore_until_ms: u64,
    input_locked_until_release: bool,
    idle_ticks: u32,
    browser_repeater: NavigationRepeater,
    reader_repeater: NavigationRepeater,
}

impl EventPump {
    pub(super) fn new() -> Self {
        Self {
            input_ignore_until_ms: now_ms() + INPUT_STARTUP_IGNORE_MS,
            input_locked_until_release: true,
            idle_ticks: 0,
            browser_repeater: NavigationRepeater::new(),
            reader_repeater: NavigationRepeater::new(),
        }
    }

    pub(super) fn poll(
        &mut self,
        hardware: &mut Hardware,
        power: &mut PowerManager,
        mode: EventMode,
    ) -> Vec<AppEvent> {
        hardware.update_inputs(self.idle_ticks);

        if hardware.has_user_activity() {
            self.idle_ticks = 0;
            power.mark_activity();
        } else {
            self.idle_ticks = self.idle_ticks.saturating_add(1);
            power.tick();
        }

        if self.inputs_are_gated(hardware) {
            return Vec::new();
        }

        let mut events = Vec::with_capacity(2);
        if hardware
            .input
            .long_pressed_once(BTN_POWER, POWER_BUTTON_SLEEP_MS)
        {
            events.push(AppEvent::PowerLongPress);
            return events;
        }

        match mode {
            EventMode::FileBrowser => self.collect_browser_events(&mut hardware.input, &mut events),
            EventMode::Reader => self.collect_reader_events(&mut hardware.input, &mut events),
        }

        if events.is_empty() && power.should_sleep() {
            events.push(AppEvent::IdleTimeout {
                idle_ticks: self.idle_ticks,
            });
        }

        events
    }

    pub(super) fn idle_ticks(&self) -> u32 {
        self.idle_ticks
    }

    fn inputs_are_gated(&mut self, hardware: &mut Hardware) -> bool {
        if now_ms() < self.input_ignore_until_ms {
            hardware.input.clear_events();
            return true;
        }

        if self.input_locked_until_release {
            hardware.input.clear_events();
            if hardware.input.any_pressed() {
                return true;
            }
            self.input_locked_until_release = false;
            return true;
        }

        false
    }

    fn collect_browser_events(&mut self, input: &mut InputManager, events: &mut Vec<AppEvent>) {
        use Button::*;

        if let Some(delta) = self.browser_repeater.navigation_delta(
            input,
            &[Down, Right],
            &[Up, Left],
            BROWSER_REPEAT_START_MS,
            BROWSER_REPEAT_INTERVAL_MS,
        ) {
            events.push(AppEvent::BrowserMove(delta));
            return;
        }

        if input.logical_was_pressed(Confirm) {
            events.push(AppEvent::BrowserConfirm);
        }
    }

    fn collect_reader_events(&mut self, input: &mut InputManager, events: &mut Vec<AppEvent>) {
        use Button::*;

        if input.logical_was_released(Back) {
            events.push(AppEvent::ReaderBack);
            return;
        }

        if let Some(delta) = self.reader_repeater.navigation_delta(
            input,
            &[PageForward, Right],
            &[PageBack, Left],
            READER_REPEAT_START_MS,
            READER_REPEAT_INTERVAL_MS,
        ) {
            events.push(AppEvent::ReaderMove(delta));
            return;
        }

        if input.logical_was_pressed(Confirm) {
            events.push(AppEvent::ReaderRefresh);
        }
    }
}

struct NavigationRepeater {
    last_repeat_ms: u64,
}

impl NavigationRepeater {
    fn new() -> Self {
        Self { last_repeat_ms: 0 }
    }

    fn navigation_delta(
        &mut self,
        input: &InputManager,
        next_buttons: &[Button],
        previous_buttons: &[Button],
        repeat_start_ms: u32,
        repeat_interval_ms: u64,
    ) -> Option<isize> {
        if input.any_logical_pressed(next_buttons) {
            self.last_repeat_ms = now_ms();
            return Some(1);
        }
        if input.any_logical_pressed(previous_buttons) {
            self.last_repeat_ms = now_ms();
            return Some(-1);
        }

        let next_held = input.any_logical_is_pressed(next_buttons);
        let previous_held = input.any_logical_is_pressed(previous_buttons);
        if !next_held && !previous_held {
            self.last_repeat_ms = 0;
            return None;
        }

        let now = now_ms();
        if now.saturating_sub(self.last_repeat_ms) < repeat_interval_ms {
            return None;
        }

        if next_held && input.any_logical_held_ms(next_buttons) >= repeat_start_ms {
            self.last_repeat_ms = now;
            return Some(1);
        }
        if previous_held && input.any_logical_held_ms(previous_buttons) >= repeat_start_ms {
            self.last_repeat_ms = now;
            return Some(-1);
        }

        None
    }
}
