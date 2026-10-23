use std::io::Cursor;

use image::{DynamicImage, ImageFormat, ImageReader, Limits, RgbaImage, imageops::FilterType};

use crate::Error;

pub const MAX_DIMENSION: u32 = 4096;
pub const MAX_ALLOCATION: u64 = 128 * 1024 * 1024;
const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

pub fn decode(data: &[u8], max_input: usize, max_side: u32) -> Result<RgbaImage, Error> {
    if data.len() > max_input || max_side == 0 || max_side > MAX_DIMENSION {
        return Err(Error::ImageLimit);
    }
    if !data.starts_with(SIGNATURE) {
        return Err(Error::InvalidImage);
    }
    let dimensions = ImageReader::with_format(Cursor::new(data), ImageFormat::Png)
        .into_dimensions()
        .map_err(|_| Error::InvalidImage)?;
    if dimensions.0 == 0
        || dimensions.1 == 0
        || dimensions.0 > MAX_DIMENSION
        || dimensions.1 > MAX_DIMENSION
    {
        return Err(Error::ImageLimit);
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_ALLOCATION);
    let mut reader = ImageReader::with_format(Cursor::new(data), ImageFormat::Png);
    reader.limits(limits);
    let image = reader.decode().map_err(|error| match error {
        image::ImageError::Limits(_) => Error::ImageLimit,
        _ => Error::InvalidImage,
    })?;
    let image = if dimensions.0 > max_side || dimensions.1 > max_side {
        image.resize(max_side, max_side, FilterType::Triangle)
    } else {
        image
    };
    Ok(image.into_rgba8())
}

pub fn sanitize(data: &[u8], max_input: usize, max_side: u32) -> Result<Vec<u8>, Error> {
    let image = decode(data, max_input, max_side)?;
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|_| Error::InvalidImage)?;
    Ok(output.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests encode generated PNG fixtures")]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, image::Rgba([12, 24, 48, 128]));
        let mut output = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut output, ImageFormat::Png)
            .unwrap();
        output.into_inner()
    }

    #[test]
    fn scales_to_rgba_and_reencodes_with_aspect_ratio() {
        let source = png(80, 40);
        let sanitized = sanitize(&source, 4096, 20).unwrap();
        let image = decode(&sanitized, 4096, 20).unwrap();
        assert_eq!(image.dimensions(), (20, 10));
        assert_eq!(image.get_pixel(0, 0).0, [12, 24, 48, 128]);
    }

    #[test]
    fn rejects_other_formats_truncation_and_resource_limits() {
        assert_eq!(decode(b"GIF89a", 1024, 96), Err(Error::InvalidImage));
        let source = png(2, 2);
        assert_eq!(decode(&source, 1, 96), Err(Error::ImageLimit));
        assert_eq!(decode(&source, 4096, 0), Err(Error::ImageLimit));
        assert!(decode(&source[..source.len() / 2], 4096, 96).is_err());
        assert_eq!(decode(&png(4097, 1), 65536, 96), Err(Error::ImageLimit));
    }
}
