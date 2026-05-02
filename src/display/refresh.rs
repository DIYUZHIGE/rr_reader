use super::{
    Display, CMD_DISPLAY_UPDATE_CTRL1, CMD_DISPLAY_UPDATE_CTRL2, CMD_MASTER_ACTIVATION,
    CMD_WRITE_LUT, CMD_WRITE_RAM_BW, CMD_WRITE_RAM_RED, CMD_WRITE_TEMPERATURE,
    PHYSICAL_DISPLAY_HEIGHT, PHYSICAL_DISPLAY_WIDTH, REFRESH_BUSY_TIMEOUT_MS,
};
use anyhow::Result;
use log::{debug, info};

// Display Update Control 1 flags
const CTRL1_NORMAL: u8 = 0x00; // Use RED RAM as previous frame for differential
const CTRL1_BYPASS_RED: u8 = 0x40; // Treat RED RAM as all-white

// Display Update Control 2 flags
const CTRL2_CLOCK: u8 = 0x80;
const CTRL2_ANALOG: u8 = 0x40;
const CTRL2_TEMP: u8 = 0x20;
const CTRL2_LUT: u8 = 0x10;
const CTRL2_MODE: u8 = 0x08;
const CTRL2_DISPLAY: u8 = 0x04;

// Full: CLOCK + ANALOG + TEMP + LUT + DISPLAY
const CTRL2_FULL: u8 = CTRL2_CLOCK | CTRL2_ANALOG | CTRL2_TEMP | CTRL2_LUT | CTRL2_DISPLAY;
// Half: CLOCK + ANALOG + LUT + DISPLAY (skip temperature load)
const CTRL2_HALF: u8 = CTRL2_CLOCK | CTRL2_ANALOG | CTRL2_LUT | CTRL2_DISPLAY;
// Fast: LUT + MODE + DISPLAY (differential, uses the controller's current
// temperature state). Do not write CMD_WRITE_TEMPERATURE immediately before
// this mode on X4: in practice that makes the update behave like a global
// refresh instead of a differential refresh.
const CTRL2_FAST: u8 = CTRL2_LUT | CTRL2_MODE | CTRL2_DISPLAY;

// High temperature value to accelerate half refresh.
const HALF_TEMPERATURE: u8 = 0x5A;
// Insert a half refresh every N fast refreshes to clear ghosting without the
// latency of a full refresh. Aggressive fast refresh uses a longer cleanup
// cadence to avoid interrupting reading flow.
const FAST_REFRESH_CLEANUP_INTERVAL: u32 = 50;

// SSD1677 custom fast LUT, adapted from h0rv/ssd1677's LUT_FAST. This is a
// deliberately short single-phase waveform for latency testing.
const FAST_LUT: [u8; 112] = [
    // VCOM
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // White -> White
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // Black -> White
    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // White -> Black
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // Black -> Black
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // VCOM DC
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // Timing
    0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshMode {
    /// Full waveform, best quality, ~1600ms. Use for boot, wakeup, ghosting cleanup.
    Full,
    /// Skip temperature load, ~900ms. Use for menu navigation.
    Half,
    /// Differential update, only changed pixels, ~400-600ms. Use for page turns.
    Fast,
}

impl Display {


    pub fn flush_if_dirty_polling<F>(&mut self, mut poll: F) -> Result<()>
    where
        F: FnMut(),
    {
        let mut poll = &mut poll as &mut dyn FnMut();
        self.flush_if_dirty_with_poll(Some(&mut poll))
    }

    fn flush_if_dirty_with_poll(&mut self, mut poll: Option<&mut dyn FnMut()>) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }

        let mode = if !self.red_ram_synced {
            RefreshMode::Half
        } else if self.fast_refresh_count >= FAST_REFRESH_CLEANUP_INTERVAL {
            debug!(
                "Inserting periodic half refresh after {} fast refreshes",
                self.fast_refresh_count
            );
            RefreshMode::Half
        } else {
            RefreshMode::Fast
        };

        self.refresh_with_poll(mode, &mut poll)?;
        self.dirty = false;
        Ok(())
    }

    /// Flush with an explicit refresh mode.
    pub fn flush_with_mode(&mut self, mode: RefreshMode) -> Result<()> {
        if self.dirty {
            self.refresh(mode)?;
            self.dirty = false;
        }
        Ok(())
    }



    fn refresh(&mut self, mode: RefreshMode) -> Result<()> {
        let mut no_poll = None;
        self.refresh_with_poll(mode, &mut no_poll)
    }

    fn refresh_with_poll(
        &mut self,
        mode: RefreshMode,
        poll: &mut Option<&mut dyn FnMut()>,
    ) -> Result<()> {
        if !self.initialized {
            self.begin()?;
        }

        debug!("Display refresh: {:?}", mode);
        self.set_ram_area(
            0,
            0,
            PHYSICAL_DISPLAY_WIDTH as u16,
            PHYSICAL_DISPLAY_HEIGHT as u16,
        )?;

        match mode {
            RefreshMode::Full => self.refresh_full(poll)?,
            RefreshMode::Half => self.refresh_half(poll)?,
            RefreshMode::Fast => self.refresh_fast(poll)?,
        }

        Ok(())
    }

    /// Full refresh: write both RAM, bypass RED, full waveform.
    fn refresh_full(&mut self, poll: &mut Option<&mut dyn FnMut()>) -> Result<()> {
        self.write_framebuffer(CMD_WRITE_RAM_BW)?;
        self.write_framebuffer(CMD_WRITE_RAM_RED)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL1)?;
        self.send_data_byte(CTRL1_BYPASS_RED)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL2)?;
        self.send_data_byte(CTRL2_FULL)?;

        self.send_command(CMD_MASTER_ACTIVATION)?;
        self.wait_while_busy("full refresh", REFRESH_BUSY_TIMEOUT_MS, poll)?;

        self.red_ram_synced = true;
        self.fast_refresh_count = 0;
        info!("Full refresh complete");
        Ok(())
    }

    /// Half refresh: write both RAM, inject high temperature, skip temp load.
    fn refresh_half(&mut self, poll: &mut Option<&mut dyn FnMut()>) -> Result<()> {
        self.write_framebuffer(CMD_WRITE_RAM_BW)?;
        self.write_framebuffer(CMD_WRITE_RAM_RED)?;

        // Inject high temperature to accelerate response
        self.send_command(CMD_WRITE_TEMPERATURE)?;
        self.send_data_byte(HALF_TEMPERATURE)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL1)?;
        self.send_data_byte(CTRL1_BYPASS_RED)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL2)?;
        self.send_data_byte(CTRL2_HALF)?;

        self.send_command(CMD_MASTER_ACTIVATION)?;
        self.wait_while_busy("half refresh", REFRESH_BUSY_TIMEOUT_MS, poll)?;

        self.red_ram_synced = true;
        self.fast_refresh_count = 0;
        info!("Half refresh complete");
        Ok(())
    }

    /// Fast (differential) refresh: write only BW RAM, compare against RED RAM.
    /// Only pixels that differ from the previous frame are driven.
    fn refresh_fast(&mut self, poll: &mut Option<&mut dyn FnMut()>) -> Result<()> {
        self.load_fast_lut()?;

        // Write new frame to BW RAM only. RED RAM keeps the previous frame
        // so the controller can compute the delta.
        self.write_framebuffer(CMD_WRITE_RAM_BW)?;

        // Normal mode: controller compares BW vs RED, only drives changed pixels
        self.send_command(CMD_DISPLAY_UPDATE_CTRL1)?;
        self.send_data_byte(CTRL1_NORMAL)?;

        self.send_command(CMD_DISPLAY_UPDATE_CTRL2)?;
        self.send_data_byte(CTRL2_FAST)?;

        self.send_command(CMD_MASTER_ACTIVATION)?;
        self.wait_while_busy("fast refresh", REFRESH_BUSY_TIMEOUT_MS, poll)?;

        // After display, copy new frame to RED RAM as baseline for next differential
        self.write_framebuffer(CMD_WRITE_RAM_RED)?;

        self.red_ram_synced = true;
        self.fast_refresh_count += 1;

        info!("Fast refresh complete (count: {})", self.fast_refresh_count);
        Ok(())
    }

    fn load_fast_lut(&mut self) -> Result<()> {
        self.send_command(CMD_WRITE_LUT)?;
        self.send_data(&FAST_LUT)
    }
}
