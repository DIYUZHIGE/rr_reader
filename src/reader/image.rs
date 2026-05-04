use anyhow::{anyhow, Result};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::sys;
use jpeg_decoder::Decoder as HeaderDecoder;
use std::ffi::CString;
use std::fs::File;
use std::io::BufReader;
use std::os::raw::c_void;

use crate::display::Display;
use crate::font::Font;

use super::{RenderImage, QUOTE_BAR_WIDTH, QUOTE_INDENT, READER_X};

const MAX_JPEG_DIMENSION: usize = 4096;

pub(super) fn draw_reader_image<F>(
    display: &mut Display,
    ui_font: &Font,
    image: &RenderImage,
    resolve_image_path: &mut F,
) where
    F: FnMut(&str) -> Result<String>,
{
    for depth in 0..image.quote_depth {
        let x = READER_X + depth * QUOTE_INDENT;
        display.fill_rect(x, image.y, QUOTE_BAR_WIDTH, image.height, 0x00);
    }

    if is_jpeg_path(&image.path) {
        if let Ok(full_path) = resolve_image_path(&image.path) {
            if draw_jpeg_streaming(display, &full_path, image).is_ok() {
                return;
            }
        }
    }

    draw_image_placeholder(display, ui_font, image);
}

fn draw_image_placeholder(display: &mut Display, ui_font: &Font, image: &RenderImage) {
    display.fill_rect(image.x, image.y, image.width, 1, 0x00);
    display.fill_rect(
        image.x,
        image.y + image.height.saturating_sub(1),
        image.width,
        1,
        0x00,
    );
    display.fill_rect(image.x, image.y, 1, image.height, 0x00);
    display.fill_rect(
        image.x + image.width.saturating_sub(1),
        image.y,
        1,
        image.height,
        0x00,
    );

    let label = if image.alt.trim().is_empty() {
        format!("[图片]\n{}", image.path)
    } else {
        format!("[图片] {}\n{}", image.alt, image.path)
    };
    display.draw_text_wrapped(
        ui_font,
        &label,
        image.x + 8,
        image.y + 8,
        image.x + image.width.saturating_sub(8),
        4,
    );
}

fn is_jpeg_path(path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

struct DrawCtx<'a> {
    display: &'a mut Display,
    draw_x: usize,
    draw_y: usize,
    max_w: usize,
    max_h: usize,
}

unsafe extern "C" fn jpeg_gray_block_cb(
    ctx: *mut c_void,
    gray: *const u8,
    left: u16,
    top: u16,
    right: u16,
    bottom: u16,
) -> i32 {
    if ctx.is_null() || gray.is_null() {
        return 0;
    }

    let ctx = &mut *(ctx as *mut DrawCtx<'_>);
    let bw = (right as usize)
        .saturating_sub(left as usize)
        .saturating_add(1);
    let bh = (bottom as usize)
        .saturating_sub(top as usize)
        .saturating_add(1);
    if bw == 0 || bh == 0 {
        return 1;
    }

    let dx = ctx.draw_x + left as usize;
    let dy = ctx.draw_y + top as usize;

    if left as usize >= ctx.max_w || top as usize >= ctx.max_h {
        return 1;
    }

    let draw_w = bw.min(ctx.max_w.saturating_sub(left as usize));
    let draw_h = bh.min(ctx.max_h.saturating_sub(top as usize));
    let src = std::slice::from_raw_parts(gray, bw.saturating_mul(bh));

    if draw_w == bw && draw_h == bh {
        ctx.display.draw_mono_bitmap(dx, dy, bw, bh, src);
        return 1;
    }

    let mut clipped = Vec::with_capacity(draw_w.saturating_mul(draw_h));
    for row in 0..draw_h {
        let start = row.saturating_mul(bw);
        let end = start.saturating_add(draw_w).min(src.len());
        clipped.extend_from_slice(&src[start..end]);
    }
    ctx.display
        .draw_mono_bitmap(dx, dy, draw_w, draw_h, &clipped);
    1
}

fn draw_jpeg_streaming(display: &mut Display, full_path: &str, image: &RenderImage) -> Result<()> {
    let (src_w, src_h) = read_jpeg_size(full_path)?;

    if src_w == 0 || src_h == 0 || src_w > MAX_JPEG_DIMENSION || src_h > MAX_JPEG_DIMENSION {
        return Err(anyhow!("jpeg dimension unsupported: {}x{}", src_w, src_h));
    }

    let scale = choose_tjpgd_scale(src_w, src_h, image.width, image.height);
    let dec_w = scaled_dim(src_w, scale);

    let draw_x = image.x + image.width.saturating_sub(dec_w) / 2;
    let draw_y = image.y;

    let c_path = CString::new(full_path).map_err(|_| anyhow!("invalid image path"))?;
    let mut out_w: u16 = 0;
    let mut out_h: u16 = 0;

    let mut ctx = DrawCtx {
        display,
        draw_x,
        draw_y,
        max_w: image.width,
        max_h: image.height,
    };

    let rc = unsafe {
        sys::rr_decode_jpeg_streaming(
            c_path.as_ptr(),
            scale,
            Some(jpeg_gray_block_cb),
            (&mut ctx as *mut DrawCtx<'_>).cast::<c_void>(),
            &mut out_w,
            &mut out_h,
        )
    };

    if rc != 0 {
        return Err(anyhow!("tjpgd decode failed: {}", rc));
    }

    FreeRtos::delay_ms(1);
    Ok(())
}

fn read_jpeg_size(path: &str) -> Result<(usize, usize)> {
    let file = File::open(path).map_err(|e| anyhow!("open {}: {}", path, e))?;
    let mut decoder = HeaderDecoder::new(BufReader::new(file));
    decoder
        .read_info()
        .map_err(|e| anyhow!("jpeg read header {}: {:?}", path, e))?;
    let info = decoder
        .info()
        .ok_or_else(|| anyhow!("jpeg header missing"))?;
    Ok((info.width as usize, info.height as usize))
}

fn choose_tjpgd_scale(src_w: usize, src_h: usize, max_w: usize, max_h: usize) -> u8 {
    for scale in 0..=3u8 {
        if scaled_dim(src_w, scale) <= max_w.max(1) && scaled_dim(src_h, scale) <= max_h.max(1) {
            return scale;
        }
    }
    3
}

fn scaled_dim(dim: usize, scale: u8) -> usize {
    let div = 1usize << scale;
    dim.saturating_add(div - 1) / div
}

// Image caching stubs -- disabled due to boot crash from jpeg decoder linking
pub fn cache_all_images(_on_progress: &mut dyn FnMut(&str)) -> usize {
    0
}

pub fn clear_image_cache() -> usize {
    0
}
