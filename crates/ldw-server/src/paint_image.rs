//! Bounded normalization for a finished browser/paper PaintResult image.
//! No original photo, metadata, or encoded user bytes leave this boundary.

use std::io::Cursor;

use png::{BitDepth, ColorType, Decoder, Encoder, Limits, SrgbRenderingIntent, Transformations};

pub const PAINT_SIDE: u32 = 512;
pub const MAX_UPLOAD_BYTES: usize = 2 * 1024 * 1024;
const MAX_DECODE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PaintImageError {
    #[error("image is not a supported single-frame 512x512 PNG")]
    InvalidImage,
    #[error("image exceeds the 2 MiB upload limit")]
    TooLarge,
}

/// Validate decoded dimensions and pixel data, then encode a fresh RGBA8 sRGB PNG.
/// The caller must separately verify template identity and authorization.
pub fn normalize_png(input: &[u8]) -> Result<Vec<u8>, PaintImageError> {
    if input.len() > MAX_UPLOAD_BYTES {
        return Err(PaintImageError::TooLarge);
    }
    if !input.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(PaintImageError::InvalidImage);
    }
    let mut decoder = Decoder::new_with_limits(
        Cursor::new(input),
        Limits {
            bytes: MAX_DECODE_BYTES,
        },
    );
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|_| PaintImageError::InvalidImage)?;
    let info = reader.info();
    if info.width != PAINT_SIDE
        || info.height != PAINT_SIDE
        || info.animation_control.is_some()
        || info.icc_profile.is_some()
        || (info.srgb.is_none() && (info.gama_chunk.is_some() || info.chrm_chunk.is_some()))
    {
        return Err(PaintImageError::InvalidImage);
    }
    let size = reader
        .output_buffer_size()
        .filter(|size| *size <= MAX_DECODE_BYTES)
        .ok_or(PaintImageError::InvalidImage)?;
    let mut decoded = vec![0; size];
    let output = reader
        .next_frame(&mut decoded)
        .map_err(|_| PaintImageError::InvalidImage)?;
    if output.width != PAINT_SIDE
        || output.height != PAINT_SIDE
        || output.bit_depth != BitDepth::Eight
    {
        return Err(PaintImageError::InvalidImage);
    }
    reader.finish().map_err(|_| PaintImageError::InvalidImage)?;
    let bytes_per_pixel = match output.color_type {
        ColorType::Grayscale => 1,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Rgb => 3,
        ColorType::Rgba => 4,
        ColorType::Indexed => return Err(PaintImageError::InvalidImage),
    };
    let pixels = (PAINT_SIDE * PAINT_SIDE) as usize;
    let source = decoded
        .get(..output.buffer_size())
        .ok_or(PaintImageError::InvalidImage)?;
    if source.len() != pixels * bytes_per_pixel {
        return Err(PaintImageError::InvalidImage);
    }
    let mut rgba = Vec::with_capacity(pixels * 4);
    for pixel in source.chunks_exact(bytes_per_pixel) {
        match output.color_type {
            ColorType::Grayscale => rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255]),
            ColorType::GrayscaleAlpha => {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
            ColorType::Rgb => rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
            ColorType::Rgba => rgba.extend_from_slice(pixel),
            ColorType::Indexed => unreachable!(),
        }
    }
    let mut normalized = Vec::new();
    {
        let mut encoder = Encoder::new(&mut normalized, PAINT_SIDE, PAINT_SIDE);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);
        encoder.set_source_srgb(SrgbRenderingIntent::Perceptual);
        let mut writer = encoder
            .write_header()
            .map_err(|_| PaintImageError::InvalidImage)?;
        writer
            .write_image_data(&rgba)
            .map_err(|_| PaintImageError::InvalidImage)?;
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, animated: bool, with_metadata: bool) -> Vec<u8> {
        let mut encoded = Vec::new();
        {
            let mut encoder = Encoder::new(&mut encoded, width, height);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(BitDepth::Eight);
            if animated {
                encoder.set_animated(2, 0).unwrap();
            }
            if with_metadata {
                encoder
                    .add_text_chunk("Comment".into(), "private photo metadata".into())
                    .unwrap();
            }
            let mut writer = encoder.write_header().unwrap();
            let pixels = vec![42u8; (width * height * 4) as usize];
            writer.write_image_data(&pixels).unwrap();
            if animated {
                writer.write_image_data(&pixels).unwrap();
            }
        }
        encoded
    }

    #[test]
    fn normalizes_pixels_and_strips_metadata() {
        let input = image(512, 512, false, true);
        let output = normalize_png(&input).unwrap();
        assert_ne!(output, input);
        assert!(
            !output
                .windows(b"private photo metadata".len())
                .any(|window| { window == b"private photo metadata" })
        );
        let mut reader = Decoder::new(Cursor::new(&output)).read_info().unwrap();
        assert_eq!(reader.info().size(), (512, 512));
        assert!(reader.info().srgb.is_some());
        assert!(reader.info().uncompressed_latin1_text.is_empty());
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(frame.color_type, ColorType::Rgba);
        assert_eq!(&pixels[..4], &[42, 42, 42, 42]);
    }

    #[test]
    fn rejects_wrong_size_animation_and_corruption() {
        assert_eq!(
            normalize_png(&image(513, 512, false, false)),
            Err(PaintImageError::InvalidImage)
        );
        assert_eq!(
            normalize_png(&image(512, 512, true, false)),
            Err(PaintImageError::InvalidImage)
        );
        let mut truncated = image(512, 512, false, false);
        truncated.truncate(truncated.len() / 2);
        assert_eq!(
            normalize_png(&truncated),
            Err(PaintImageError::InvalidImage)
        );
        assert_eq!(
            normalize_png(&vec![0; MAX_UPLOAD_BYTES + 1]),
            Err(PaintImageError::TooLarge)
        );
    }
}
