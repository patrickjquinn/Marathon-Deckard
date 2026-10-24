//! Re-encoding of untrusted PNGs within fixed resource limits.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits, RgbaImage};

const MAX_DIMENSION: u32 = 4096;
const MAX_ALLOCATION: u64 = 128 * 1024 * 1024;

/// `data`, at most `max_input` bytes of PNG, decoded within the limits and
/// scaled to fit `max_side`. `None` when it is not a PNG or breaks a limit.
#[must_use]
pub fn decode(data: &[u8], max_input: usize, max_side: u32) -> Option<RgbaImage> {
    if data.len() > max_input {
        return None;
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_ALLOCATION);
    let mut reader = ImageReader::with_format(Cursor::new(data), ImageFormat::Png);
    reader.limits(limits);
    let mut image = reader.decode().ok()?;
    if image.width() > max_side || image.height() > max_side {
        image = image.resize(max_side, max_side, FilterType::Triangle);
    }
    Some(image.into_rgba8())
}

/// [`decode`], re-encoded as RGBA PNG.
#[must_use]
pub fn sanitize(data: &[u8], max_input: usize, max_side: u32) -> Option<Vec<u8>> {
    let mut output = Cursor::new(Vec::new());
    let image = decode(data, max_input, max_side)?;
    image.write_to(&mut output, ImageFormat::Png).ok()?;
    Some(output.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests encode generated PNG fixtures")]
mod tests {
    use super::*;
    use image::Rgba;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, Rgba([12, 24, 48, 128]));
        let mut output = Cursor::new(Vec::new());
        image.write_to(&mut output, ImageFormat::Png).unwrap();
        output.into_inner()
    }

    #[test]
    fn scales_to_rgba_and_reencodes_with_aspect_ratio() {
        let sanitized = sanitize(&png(80, 40), 4096, 20).unwrap();
        let image = image::load_from_memory(&sanitized).unwrap().into_rgba8();
        assert_eq!(image.dimensions(), (20, 10));
        assert_eq!(image.get_pixel(0, 0).0, [12, 24, 48, 128]);
    }

    #[test]
    fn rejects_other_formats_truncation_and_resource_limits() {
        assert!(sanitize(b"GIF89a", 1024, 96).is_none());
        let source = png(2, 2);
        assert!(sanitize(&source, 1, 96).is_none());
        assert!(sanitize(&source[..source.len() / 2], 4096, 96).is_none());
        assert!(sanitize(&png(4097, 1), 65536, 96).is_none());
    }
}
