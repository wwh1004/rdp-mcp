use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy)]
pub struct NativeRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub struct EncodedImage {
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
    pub width: u32,
    pub height: u32,
}

pub fn encode(
    rgba: &[u8],
    native_width: u32,
    native_height: u32,
    format: &str,
    quality: u8,
    max_width: Option<u32>,
    region: Option<NativeRegion>,
) -> Result<EncodedImage> {
    let expected = native_width as usize * native_height as usize * 4;
    if native_width == 0 || native_height == 0 || rgba.len() != expected {
        bail!("invalid native framebuffer");
    }

    let region = clamp_region(region, native_width, native_height)?;
    let mut pixels = crop_rgba(rgba, native_width, region);
    let mut width = region.width;
    let mut height = region.height;

    if let Some(limit) = max_width.filter(|limit| *limit > 0 && width > *limit) {
        let target_height =
            ((height as u64 * limit as u64 + width as u64 / 2) / width as u64).max(1) as u32;
        pixels = resize_nearest(&pixels, width, height, limit, target_height);
        width = limit;
        height = target_height;
    }

    match format.to_ascii_lowercase().as_str() {
        "jpeg" | "jpg" => {
            let rgb = rgba_to_rgb(&pixels);
            let mut bytes = Vec::new();
            jpeg_encoder::Encoder::new(&mut bytes, quality.clamp(1, 100)).encode(
                &rgb,
                width as u16,
                height as u16,
                jpeg_encoder::ColorType::Rgb,
            )?;
            Ok(EncodedImage {
                bytes,
                mime_type: "image/jpeg",
                width,
                height,
            })
        }
        "png" => {
            let mut bytes = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut bytes, width, height);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header()?;
                writer.write_image_data(&pixels)?;
            }
            Ok(EncodedImage {
                bytes,
                mime_type: "image/png",
                width,
                height,
            })
        }
        _ => bail!("image format must be 'png' or 'jpeg'"),
    }
}

fn clamp_region(requested: Option<NativeRegion>, width: u32, height: u32) -> Result<NativeRegion> {
    let Some(region) = requested else {
        return Ok(NativeRegion {
            x: 0,
            y: 0,
            width,
            height,
        });
    };
    if region.width == 0 || region.height == 0 || region.x >= width || region.y >= height {
        bail!("screenshot region is outside the desktop");
    }
    Ok(NativeRegion {
        x: region.x,
        y: region.y,
        width: region.width.min(width - region.x),
        height: region.height.min(height - region.y),
    })
}

fn crop_rgba(source: &[u8], source_width: u32, region: NativeRegion) -> Vec<u8> {
    let row_bytes = region.width as usize * 4;
    let source_stride = source_width as usize * 4;
    let mut output = vec![0; row_bytes * region.height as usize];
    for row in 0..region.height as usize {
        let source_start = (region.y as usize + row) * source_stride + region.x as usize * 4;
        let target_start = row * row_bytes;
        output[target_start..target_start + row_bytes]
            .copy_from_slice(&source[source_start..source_start + row_bytes]);
    }
    output
}

fn resize_nearest(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<u8> {
    let mut output = vec![0; target_width as usize * target_height as usize * 4];
    for target_y in 0..target_height {
        let source_y = target_y as u64 * source_height as u64 / target_height as u64;
        for target_x in 0..target_width {
            let source_x = target_x as u64 * source_width as u64 / target_width as u64;
            let source_offset = (source_y as usize * source_width as usize + source_x as usize) * 4;
            let target_offset = (target_y as usize * target_width as usize + target_x as usize) * 4;
            output[target_offset..target_offset + 4]
                .copy_from_slice(&source[source_offset..source_offset + 4]);
        }
    }
    output
}

fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for pixel in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_supported_formats() {
        let rgba = [255, 0, 0, 255, 0, 255, 0, 255];
        let jpeg = encode(&rgba, 2, 1, "jpeg", 60, None, None).unwrap();
        assert_eq!(&jpeg.bytes[..2], &[0xff, 0xd8]);
        let png = encode(&rgba, 2, 1, "png", 60, None, None).unwrap();
        assert_eq!(&png.bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
}
