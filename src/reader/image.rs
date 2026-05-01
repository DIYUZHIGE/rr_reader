use anyhow::{anyhow, Result};
use esp_idf_hal::delay::FreeRtos;
use std::io::{BufRead, Read, Seek};
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::display::Display;
use crate::font::Font;

use super::{RenderImage, QUOTE_BAR_WIDTH, QUOTE_INDENT, READER_X};

const JPEG_DECODE_MAX_WIDTH: usize = 256;
const JPEG_DECODE_MAX_HEIGHT: usize = 256;
const MAX_JPEG_DIMENSION: u16 = 4096;
/// Max pixels for the grayscale decode buffer (1 byte/pixel).
/// 64KB allows up to 256x256 full decode.
const MAX_DECODE_PIXELS: usize = 64 * 1024;

#[derive(Debug)]
struct DecodedImage {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

pub(super) fn draw_reader_image<F, R>(
    display: &mut Display,
    ui_font: &Font,
    image: &RenderImage,
    load_image: &mut F,
) where
    F: FnMut(&str) -> Result<R>,
    R: BufRead + Seek,
{
    for depth in 0..image.quote_depth {
        let x = READER_X + depth * QUOTE_INDENT;
        display.fill_rect(x, image.y, QUOTE_BAR_WIDTH, image.height, 0x00);
    }

    if is_jpeg_path(&image.path) {
        if let Ok(decoded) = load_image(&image.path)
            .and_then(|reader| decode_jpeg_to_mono(reader, image.width, image.height))
        {
            let (target_width, target_height) = fit_dimensions_allow_upscale(
                decoded.width,
                decoded.height,
                image.width,
                image.height,
            );
            let x = image.x + image.width.saturating_sub(target_width) / 2;
            display.draw_mono_bitmap_scaled(
                x,
                image.y,
                (decoded.width, decoded.height),
                (target_width, target_height),
                &decoded.pixels,
            );
            return;
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

fn decode_jpeg_to_mono<R: BufRead + Seek>(
    reader: R,
    max_width: usize,
    max_height: usize,
) -> Result<DecodedImage> {
    FreeRtos::delay_ms(1);

    // Use grayscale output (Luma) for 1 byte/pixel -- 3x less memory than RGB.
    let options = DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::Luma);

    let mut decoder = JpegDecoder::new_with_options(reader, options);

    decoder
        .decode_headers()
        .map_err(|e| anyhow!("jpeg decode headers: {:?}", e))?;

    let info = decoder.info().ok_or_else(|| anyhow!("jpeg info missing"))?;

    let src_w = info.width as usize;
    let src_h = info.height as usize;

    if info.width > MAX_JPEG_DIMENSION || info.height > MAX_JPEG_DIMENSION {
        return Err(anyhow!(
            "jpeg dimensions too large: {}x{}",
            info.width,
            info.height
        ));
    }

    if src_w == 0 || src_h == 0 {
        return Err(anyhow!("jpeg has zero dimension"));
    }

    // Check that grayscale output fits in our decode buffer.
    let decode_pixels = src_w.saturating_mul(src_h);
    if decode_pixels > MAX_DECODE_PIXELS {
        return Err(anyhow!(
            "jpeg too large for decode buffer: {}x{} = {} px (limit {})",
            src_w,
            src_h,
            decode_pixels,
            MAX_DECODE_PIXELS
        ));
    }

    // Decode directly to grayscale.
    FreeRtos::delay_ms(1);
    let gray = decoder
        .decode()
        .map_err(|e| anyhow!("jpeg decode: {:?}", e))?;
    FreeRtos::delay_ms(1);

    if gray.len() < src_w * src_h {
        return Err(anyhow!(
            "jpeg output too small: {} bytes, expected {}",
            gray.len(),
            src_w * src_h
        ));
    }

    let decode_max_width = max_width.clamp(1, JPEG_DECODE_MAX_WIDTH);
    let decode_max_height = max_height.clamp(1, JPEG_DECODE_MAX_HEIGHT);

    let (target_width, target_height) =
        fit_dimensions(src_w, src_h, decode_max_width, decode_max_height);

    // Convert grayscale to mono (threshold) + nearest-neighbour scale in one pass.
    let mono = gray_to_mono_nearest(&gray, src_w, src_h, target_width, target_height)?;

    Ok(DecodedImage {
        width: target_width,
        height: target_height,
        pixels: mono,
    })
}

/// Downscale dimensions to fit within `max_w x max_h`, preserving aspect ratio.
fn fit_dimensions(src_w: usize, src_h: usize, max_w: usize, max_h: usize) -> (usize, usize) {
    let src_w = src_w.max(1);
    let src_h = src_h.max(1);
    let max_w = max_w.max(1);
    let max_h = max_h.max(1);

    if src_w <= max_w && src_h <= max_h {
        return (src_w, src_h);
    }

    let h_by_w = src_h.saturating_mul(max_w) / src_w;
    if h_by_w <= max_h {
        (max_w, h_by_w.max(1))
    } else {
        (src_w.saturating_mul(max_h) / src_h.max(1), max_h)
    }
}

/// Fit dimensions into `max_w x max_h`, allowing upscale for images smaller than the max.
fn fit_dimensions_allow_upscale(
    src_w: usize,
    src_h: usize,
    max_w: usize,
    max_h: usize,
) -> (usize, usize) {
    let src_w = src_w.max(1);
    let src_h = src_h.max(1);
    let max_w = max_w.max(1);
    let max_h = max_h.max(1);

    let h_by_w = src_h.saturating_mul(max_w) / src_w;
    if h_by_w <= max_h {
        (max_w, h_by_w.max(1))
    } else {
        (src_w.saturating_mul(max_h) / src_h, max_h)
    }
}

/// Convert a grayscale (Luma) buffer to mono (black/white) with nearest-neighbour
/// downscaling. Returns a fresh `Vec` so the caller can drop the large decode
/// buffer as soon as the conversion finishes.
fn gray_to_mono_nearest(
    gray: &[u8],
    src_w: usize,
    src_h: usize,
    target_w: usize,
    target_h: usize,
) -> Result<Vec<u8>> {
    let target_len = target_w.saturating_mul(target_h);
    let mut mono = Vec::with_capacity(target_len);

    for y in 0..target_h {
        let src_y = y.saturating_mul(src_h) / target_h;
        let row_start = src_y.saturating_mul(src_w);

        for x in 0..target_w {
            let src_x = x.saturating_mul(src_w) / target_w;
            let gray_val = gray.get(row_start + src_x).copied().unwrap_or(0xFF);
            // Threshold: < 150 -> black, >= 150 -> white.
            // Matches the previous jpeg-decoder behaviour.
            mono.push(if gray_val < 150 { 0x00 } else { 0xFF });
        }

        // Yield to FreeRTOS every 24 rows so the watchdog doesn't fire.
        if y % 24 == 0 {
            FreeRtos::delay_ms(1);
        }
    }

    Ok(mono)
}
