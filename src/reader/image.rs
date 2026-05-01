use anyhow::{anyhow, Result};
use esp_idf_hal::delay::FreeRtos;
use jpeg_decoder::{ColorTransform, Decoder as JpegDecoder, PixelFormat};
use std::io::{Read, Seek};

use crate::display::Display;
use crate::font::Font;

use super::{RenderImage, QUOTE_BAR_WIDTH, QUOTE_INDENT, READER_X};

const JPEG_DECODE_MAX_WIDTH: usize = 256;
const JPEG_DECODE_MAX_HEIGHT: usize = 256;
const MAX_JPEG_DIMENSION: u16 = 4096;
// Limit JPEG decode buffer to 64KB. This allows ~256×256 mono output while
// rejecting images that need more. Photos that exceed this will show a placeholder.
const MAX_JPEG_DECODE_BUFFER_BYTES: usize = 64 * 1024;

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
    R: Read + Seek,
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

fn decode_jpeg_to_mono<R: Read + Seek>(
    reader: R,
    max_width: usize,
    max_height: usize,
) -> Result<DecodedImage> {
    FreeRtos::delay_ms(1);
    let mut decoder = JpegDecoder::new(reader);
    decoder.set_max_decoding_buffer_size(MAX_JPEG_DECODE_BUFFER_BYTES);
    decoder.set_color_transform(ColorTransform::Grayscale);
    decoder
        .read_info()
        .map_err(|e| anyhow!("read jpeg info: {}", e))?;
    let info = decoder.info().ok_or_else(|| anyhow!("jpeg info missing"))?;
    if info.width > MAX_JPEG_DIMENSION || info.height > MAX_JPEG_DIMENSION {
        return Err(anyhow!(
            "jpeg dimensions too large: {}x{}",
            info.width,
            info.height
        ));
    }

    let decode_max_width = max_width.clamp(1, JPEG_DECODE_MAX_WIDTH);
    let decode_max_height = max_height.clamp(1, JPEG_DECODE_MAX_HEIGHT);
    let source_channels = info.pixel_format.pixel_bytes().max(1);
    let requested = jpeg_scale_request(
        info.width,
        info.height,
        decode_max_width,
        decode_max_height,
        source_channels,
        MAX_JPEG_DECODE_BUFFER_BYTES,
    );
    let (scaled_width, scaled_height) = decoder
        .scale(requested.0, requested.1)
        .map_err(|e| anyhow!("scale jpeg: {}", e))?;
    if usize::from(scaled_width)
        .saturating_mul(usize::from(scaled_height))
        .saturating_mul(3)
        > MAX_JPEG_DECODE_BUFFER_BYTES
    {
        return Err(anyhow!(
            "scaled jpeg buffer too large: {}x{}",
            scaled_width,
            scaled_height
        ));
    }

    FreeRtos::delay_ms(1);
    let decoded = decoder
        .decode()
        .map_err(|e| anyhow!("decode jpeg: {}", e))?;
    FreeRtos::delay_ms(1);
    let info = decoder
        .info()
        .ok_or_else(|| anyhow!("decoded jpeg info missing"))?;
    let source_width = usize::from(info.width);
    let source_height = usize::from(info.height);
    let (source_channels, pixel_format) = if decoded.len() == source_width * source_height {
        (1, PixelFormat::L8)
    } else {
        (info.pixel_format.pixel_bytes(), info.pixel_format)
    };
    if source_width == 0 || source_height == 0 || source_channels == 0 {
        return Err(anyhow!("invalid jpeg output"));
    }

    let (target_width, target_height) = fit_dimensions(
        source_width,
        source_height,
        decode_max_width,
        decode_max_height,
    );
    let pixels = jpeg_to_mono_nearest(
        decoded,
        source_width,
        source_height,
        source_channels,
        pixel_format,
        target_width,
        target_height,
    )?;

    Ok(DecodedImage {
        width: target_width,
        height: target_height,
        pixels,
    })
}

fn jpeg_scale_request(
    source_width: u16,
    source_height: u16,
    max_width: usize,
    max_height: usize,
    source_channels: usize,
    max_buffer_bytes: usize,
) -> (u16, u16) {
    let max_width = max_width.max(1);
    let max_height = max_height.max(1);
    let source_channels = source_channels.max(1);
    let mut best = (1u16, 1u16);

    for idct_scale in [1usize, 2, 4, 8] {
        let width = scaled_jpeg_dimension(source_width, idct_scale);
        let height = scaled_jpeg_dimension(source_height, idct_scale);
        let fits_view = usize::from(width) <= max_width && usize::from(height) <= max_height;
        let fits_memory = usize::from(width)
            .saturating_mul(usize::from(height))
            .saturating_mul(source_channels)
            <= max_buffer_bytes;

        if fits_view && fits_memory {
            best = (width, height);
        }
    }

    best
}

fn scaled_jpeg_dimension(length: u16, idct_scale: usize) -> u16 {
    ((usize::from(length) * idct_scale).saturating_sub(1) / 8 + 1)
        .min(u16::MAX as usize)
        .max(1) as u16
}

fn fit_dimensions(
    source_width: usize,
    source_height: usize,
    max_width: usize,
    max_height: usize,
) -> (usize, usize) {
    if source_width <= max_width && source_height <= max_height {
        return (source_width.max(1), source_height.max(1));
    }

    let width_limited_height = source_height.saturating_mul(max_width) / source_width.max(1);
    if width_limited_height <= max_height {
        (max_width.max(1), width_limited_height.max(1))
    } else {
        let width = source_width.saturating_mul(max_height) / source_height.max(1);
        (width.max(1), max_height.max(1))
    }
}

fn fit_dimensions_allow_upscale(
    source_width: usize,
    source_height: usize,
    max_width: usize,
    max_height: usize,
) -> (usize, usize) {
    let source_width = source_width.max(1);
    let source_height = source_height.max(1);
    let max_width = max_width.max(1);
    let max_height = max_height.max(1);

    let width_limited_height = source_height.saturating_mul(max_width) / source_width;
    if width_limited_height <= max_height {
        (max_width, width_limited_height.max(1))
    } else {
        let width = source_width.saturating_mul(max_height) / source_height;
        (width.max(1), max_height)
    }
}

fn jpeg_to_mono_nearest(
    mut decoded: Vec<u8>,
    source_width: usize,
    source_height: usize,
    source_channels: usize,
    pixel_format: PixelFormat,
    target_width: usize,
    target_height: usize,
) -> Result<Vec<u8>> {
    let target_len = target_width.saturating_mul(target_height);
    if target_len > decoded.len() {
        return Err(anyhow!(
            "jpeg output buffer too small for mono conversion: {} < {}",
            decoded.len(),
            target_len
        ));
    }

    for y in 0..target_height {
        let source_y = y * source_height / target_height;
        for x in 0..target_width {
            let source_x = x * source_width / target_width;
            let source_index = (source_y * source_width + source_x) * source_channels;
            let gray = jpeg_gray_at(&decoded, source_index, pixel_format)?;
            decoded[y * target_width + x] = if gray < 150 { 0x00 } else { 0xFF };
        }
        if y % 24 == 0 {
            FreeRtos::delay_ms(1);
        }
    }

    decoded.truncate(target_len);
    Ok(decoded)
}

fn jpeg_gray_at(decoded: &[u8], index: usize, pixel_format: PixelFormat) -> Result<u8> {
    match pixel_format {
        PixelFormat::L8 => decoded
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("jpeg luma pixel out of range")),
        PixelFormat::RGB24 => {
            let r = *decoded
                .get(index)
                .ok_or_else(|| anyhow!("jpeg red pixel out of range"))? as u16;
            let g = *decoded
                .get(index + 1)
                .ok_or_else(|| anyhow!("jpeg green pixel out of range"))?
                as u16;
            let b = *decoded
                .get(index + 2)
                .ok_or_else(|| anyhow!("jpeg blue pixel out of range"))? as u16;
            Ok(((r * 77 + g * 150 + b * 29) >> 8) as u8)
        }
        PixelFormat::CMYK32 => {
            let c = *decoded
                .get(index)
                .ok_or_else(|| anyhow!("jpeg cyan pixel out of range"))? as u16;
            let m = *decoded
                .get(index + 1)
                .ok_or_else(|| anyhow!("jpeg magenta pixel out of range"))?
                as u16;
            let y = *decoded
                .get(index + 2)
                .ok_or_else(|| anyhow!("jpeg yellow pixel out of range"))?
                as u16;
            let k = *decoded
                .get(index + 3)
                .ok_or_else(|| anyhow!("jpeg black pixel out of range"))?
                as u16;
            let r = 255u16.saturating_sub((c + k).min(255));
            let g = 255u16.saturating_sub((m + k).min(255));
            let b = 255u16.saturating_sub((y + k).min(255));
            Ok(((r * 77 + g * 150 + b * 29) >> 8) as u8)
        }
        PixelFormat::L16 => decoded
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("jpeg l16 pixel out of range")),
    }
}
