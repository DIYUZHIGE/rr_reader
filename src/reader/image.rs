use anyhow::{anyhow, Result};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::sys;
use jpeg_decoder::Decoder as HeaderDecoder;
use std::ffi::CString;
use std::fs::File;
use std::io::{BufReader, Write};
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
            // Try cache first
            if let Ok((w, h, mono)) = load_img_cache(&full_path, image.width, image.height) {
                draw_packed_to_display(display, image, w, h, &mono);
                return;
            }
            // Decode and cache
            if let Ok((w, h, mono)) = decode_jpeg_to_mono(&full_path, image.width, image.height) {
                save_img_cache(&img_cache_path(&full_path), w, h, &mono);
                draw_packed_to_display(display, image, w, h, &mono);
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

fn draw_packed_to_display(
    display: &mut Display,
    image: &RenderImage,
    w: usize,
    h: usize,
    packed: &[u8],
) {
    let stride = (w + 7) / 8;
    let dx = image.x + image.width.saturating_sub(w) / 2;
    let dy = image.y;
    for py in 0..h {
        for px in 0..w {
            let byte_off = py * stride + px / 8;
            let bit_off = px % 8;
            if let Some(&byte) = packed.get(byte_off) {
                let black = (byte & (0x80 >> bit_off)) != 0;
                display.set_pixel(dx + px, dy + py, !black);
            }
        }
    }
    display.mark_dirty();
}

const IMG_CACHE_DIR: &str = "/sdcard/vault/.rr_cache";

struct DrawCtx<'a> {
    display: Option<&'a mut Display>,
    draw_x: usize,
    draw_y: usize,
    max_w: usize,
    max_h: usize,
    // Buffer mode: if set, write packed bits here instead of display
    buf: *mut u8,
    buf_stride: usize,
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

    if left as usize >= ctx.max_w || top as usize >= ctx.max_h {
        return 1;
    }

    // Buffer mode
    if !ctx.buf.is_null() {
        let src = std::slice::from_raw_parts(gray, bw.saturating_mul(bh));
        for row in 0..bh {
            let y = top as usize + row;
            let byte_off = y * ctx.buf_stride + (left as usize / 8);
            let bit_off = left as usize % 8;
            let dst = &mut *ctx.buf.add(byte_off);
            for col in 0..bw {
                if src[row * bw + col] <= 128 {
                    *dst |= 0x80 >> ((bit_off + col) % 8);
                }
            }
        }
        return 1;
    }

    // Display mode
    let display = ctx.display.as_deref_mut().unwrap();
    let dx = ctx.draw_x + left as usize;
    let dy = ctx.draw_y + top as usize;

    let draw_w = bw.min(ctx.max_w.saturating_sub(left as usize));
    let draw_h = bh.min(ctx.max_h.saturating_sub(top as usize));
    let src = std::slice::from_raw_parts(gray, bw.saturating_mul(bh));

    if draw_w == bw && draw_h == bh {
        display.draw_mono_bitmap(dx, dy, bw, bh, src);
        return 1;
    }

    let mut clipped = Vec::with_capacity(draw_w.saturating_mul(draw_h));
    for row in 0..draw_h {
        let start = row.saturating_mul(bw);
        let end = start.saturating_add(draw_w).min(src.len());
        clipped.extend_from_slice(&src[start..end]);
    }
    display.draw_mono_bitmap(dx, dy, draw_w, draw_h, &clipped);
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
        display: Some(display),
        draw_x,
        draw_y,
        max_w: image.width,
        max_h: image.height,
        buf: std::ptr::null_mut(),
        buf_stride: 0,
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

fn decode_jpeg_to_mono(
    full_path: &str,
    max_w: usize,
    max_h: usize,
) -> Result<(usize, usize, Vec<u8>)> {
    let (src_w, src_h) = read_jpeg_size(full_path)?;
    if src_w == 0 || src_h == 0 || src_w > MAX_JPEG_DIMENSION || src_h > MAX_JPEG_DIMENSION {
        return Err(anyhow!("jpeg dimension unsupported"));
    }
    let scale = choose_tjpgd_scale(src_w, src_h, max_w, max_h);
    let dec_w = scaled_dim(src_w, scale);
    let dec_h = scaled_dim(src_h, scale);
    let stride = (dec_w + 7) / 8;
    let mut buffer = vec![0u8; stride * dec_h];

    let c_path = CString::new(full_path).map_err(|_| anyhow!("invalid path"))?;
    let mut ctx = DrawCtx {
        display: None,
        draw_x: 0,
        draw_y: 0,
        max_w: dec_w,
        max_h: dec_h,
        buf: buffer.as_mut_ptr(),
        buf_stride: stride,
    };

    let rc = unsafe {
        sys::rr_decode_jpeg_streaming(
            c_path.as_ptr(),
            scale,
            Some(jpeg_gray_block_cb),
            (&mut ctx as *mut DrawCtx<'_>).cast::<c_void>(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(anyhow!("tjpgd decode failed: {}", rc));
    }
    Ok((dec_w, dec_h, buffer))
}

fn img_hash(path: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

fn img_cache_path(file_path: &str) -> String {
    format!("{}/{:016x}.img", IMG_CACHE_DIR, img_hash(file_path))
}

fn save_img_cache(cpath: &str, w: usize, h: usize, mono: &[u8]) {
    let _ = std::fs::create_dir_all(IMG_CACHE_DIR);
    if let Ok(mut f) = File::create(cpath) {
        let _ = f.write_all(&(w as u16).to_le_bytes());
        let _ = f.write_all(&(h as u16).to_le_bytes());
        let _ = f.write_all(mono);
    }
}

fn load_img_cache(full_path: &str, max_w: usize, max_h: usize) -> Result<(usize, usize, Vec<u8>)> {
    let cpath = img_cache_path(full_path);
    if let Ok(data) = std::fs::read(&cpath) {
        if data.len() >= 4 {
            let w = u16::from_le_bytes([data[0], data[1]]) as usize;
            let h = u16::from_le_bytes([data[2], data[3]]) as usize;
            let expected = 4 + (w * h + 7) / 8;
            if w > 0 && h > 0 && w <= max_w && h <= max_h && data.len() == expected {
                return Ok((w, h, data[4..].to_vec()));
            }
        }
        // Stale cache, remove it
        let _ = std::fs::remove_file(&cpath);
    }
    Err(anyhow!("cache miss"))
}

// --- Image caching stubs (functions use same callback, no new extern "C") ---
pub fn cache_all_images(_on_progress: &mut dyn FnMut(&str)) -> usize {
    0
}

pub fn clear_image_cache() -> usize {
    0
}
