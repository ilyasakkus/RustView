//! Bounded JPEG encoding and decoding for desktop frames.
//!
//! A remote peer controls the encoded bytes, so decoding must validate both the
//! compressed payload and the advertised dimensions before allocating a pixel
//! buffer. The limits in this module are deliberately shared by the encoder and
//! decoder so that every locally produced frame is acceptable to a peer running
//! the same RustView version.

use std::io::Cursor;

use anyhow::{Context, Result, bail};
use image::{
    ColorType, ImageDecoder, RgbImage, RgbaImage,
    codecs::jpeg::{JpegDecoder, JpegEncoder},
    imageops::{FilterType, resize},
};

/// Largest width sent by the JPEG MVP transport.
pub const MAX_FRAME_WIDTH: u32 = 1_280;

/// A secondary dimension ceiling that rejects implausibly tall JPEG headers.
///
/// The decoded-byte budget below is normally the tighter constraint. Keeping a
/// separate ceiling also prevents pathological one-pixel-wide images.
pub const MAX_FRAME_HEIGHT: u32 = 4_096;

/// Maximum compressed payload and maximum prospective RGBA allocation.
pub const MAX_FRAME_BYTES: usize = rustview_core::protocol::MAX_JPEG_FRAME_SIZE;

const RGBA_BYTES_PER_PIXEL: u64 = 4;
const SCALE_DENOMINATOR: u64 = 1_000_000;

/// A JPEG frame ready to put into a RustView protocol message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedFrame {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// Resize an RGBA capture to the transport limits while preserving its aspect
/// ratio.
///
/// Images that already fit are cloned unchanged. Besides the width ceiling,
/// the result is constrained so converting it to RGBA on the receiver can never
/// require more than [`MAX_FRAME_BYTES`].
pub fn resize_for_transport(image: &RgbaImage) -> Result<RgbaImage> {
    let (width, height) = image.dimensions();
    validate_non_zero_dimensions(width, height)?;

    let (target_width, target_height) = fitted_dimensions(width, height);
    validate_transport_dimensions(target_width, target_height)?;

    if (target_width, target_height) == (width, height) {
        return Ok(image.clone());
    }

    Ok(resize(
        image,
        target_width,
        target_height,
        FilterType::Triangle,
    ))
}

/// Resize and encode an RGBA capture as a bounded JPEG frame.
pub fn encode_jpeg(image: &RgbaImage, quality: u8) -> Result<EncodedFrame> {
    if !(1..=100).contains(&quality) {
        bail!("JPEG quality must be in 1..=100, got {quality}");
    }

    let resized = resize_for_transport(image)?;
    let (width, height) = resized.dimensions();
    let rgb = image::DynamicImage::ImageRgba8(resized).into_rgb8();
    let mut bytes = Vec::new();

    JpegEncoder::new_with_quality(&mut bytes, quality)
        .encode(rgb.as_raw(), width, height, ColorType::Rgb8.into())
        .context("failed to encode desktop frame as JPEG")?;

    if bytes.len() > MAX_FRAME_BYTES {
        bail!(
            "encoded JPEG is {} bytes; limit is {MAX_FRAME_BYTES}",
            bytes.len()
        );
    }

    Ok(EncodedFrame {
        width,
        height,
        bytes,
    })
}

/// Decode a JPEG payload after enforcing compressed-size and dimension limits.
///
/// The returned pixels are always RGB8. JPEGs that advertise dimensions which
/// would exceed an 8 MiB RGBA upload are rejected before the output allocation.
pub fn decode_jpeg(bytes: &[u8]) -> Result<RgbImage> {
    validate_jpeg_payload(bytes)?;

    let decoder = JpegDecoder::new(Cursor::new(bytes)).context("invalid JPEG frame")?;
    let (width, height) = decoder.dimensions();
    validate_transport_dimensions(width, height)?;

    let color_type = decoder.color_type();
    let decoded_len = usize::try_from(decoder.total_bytes())
        .context("decoded JPEG byte count does not fit this platform")?;
    if decoded_len > MAX_FRAME_BYTES {
        bail!("decoded JPEG requires {decoded_len} bytes; limit is {MAX_FRAME_BYTES}");
    }

    let mut decoded = vec![0_u8; decoded_len];
    decoder
        .read_image(&mut decoded)
        .context("failed to decode JPEG frame")?;

    match color_type {
        ColorType::Rgb8 => RgbImage::from_raw(width, height, decoded)
            .context("JPEG decoder returned an invalid RGB buffer"),
        ColorType::L8 => {
            let gray = image::GrayImage::from_raw(width, height, decoded)
                .context("JPEG decoder returned an invalid grayscale buffer")?;
            Ok(image::DynamicImage::ImageLuma8(gray).into_rgb8())
        }
        other => bail!("unsupported JPEG output color type: {other:?}"),
    }
}

fn validate_jpeg_payload(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_FRAME_BYTES {
        bail!(
            "JPEG payload is {} bytes; limit is {MAX_FRAME_BYTES}",
            bytes.len()
        );
    }
    if bytes.len() < 4 {
        bail!("JPEG payload is too short");
    }
    if !bytes.starts_with(&[0xff, 0xd8]) || !bytes.ends_with(&[0xff, 0xd9]) {
        bail!("JPEG payload must contain exact SOI and EOI markers");
    }
    Ok(())
}

fn validate_non_zero_dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 {
        bail!("frame dimensions must be non-zero, got {width}x{height}");
    }
    Ok(())
}

fn validate_transport_dimensions(width: u32, height: u32) -> Result<()> {
    validate_non_zero_dimensions(width, height)?;

    if width > MAX_FRAME_WIDTH {
        bail!("frame width {width} exceeds limit {MAX_FRAME_WIDTH}");
    }
    if height > MAX_FRAME_HEIGHT {
        bail!("frame height {height} exceeds limit {MAX_FRAME_HEIGHT}");
    }

    let rgba_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(RGBA_BYTES_PER_PIXEL))
        .context("frame dimensions overflow the decoded-size calculation")?;
    let byte_limit = u64::try_from(MAX_FRAME_BYTES).expect("8 MiB fits in u64");
    if rgba_bytes > byte_limit {
        bail!("frame {width}x{height} needs {rgba_bytes} RGBA bytes; limit is {MAX_FRAME_BYTES}");
    }

    Ok(())
}

fn fitted_dimensions(width: u32, height: u32) -> (u32, u32) {
    if validate_transport_dimensions(width, height).is_ok() {
        return (width, height);
    }

    // Find the largest fixed-point scale that satisfies every transport limit.
    // Integer arithmetic avoids platform-dependent floating-point rounding at
    // the exact width and byte-budget boundaries.
    let mut low = 1_u64;
    let mut high = SCALE_DENOMINATOR;
    let mut best = (1_u32, 1_u32);

    while low <= high {
        let scale = low + (high - low) / 2;
        let candidate = (
            scaled_dimension(width, scale),
            scaled_dimension(height, scale),
        );

        if validate_transport_dimensions(candidate.0, candidate.1).is_ok() {
            best = candidate;
            low = scale + 1;
        } else {
            high = scale - 1;
        }
    }

    best
}

fn scaled_dimension(value: u32, scale: u64) -> u32 {
    let scaled = u64::from(value)
        .saturating_mul(scale)
        .checked_div(SCALE_DENOMINATOR)
        .unwrap_or(0)
        .max(1);
    u32::try_from(scaled).expect("a downscaled u32 dimension still fits in u32")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, Rgba};

    #[test]
    fn resize_caps_width_and_preserves_aspect_ratio() {
        let source = RgbaImage::from_pixel(2_000, 1_000, Rgba([20, 40, 60, 255]));

        let resized = resize_for_transport(&source).expect("resize should succeed");

        assert_eq!(resized.dimensions(), (1_280, 640));
    }

    #[test]
    fn resize_also_obeys_decoded_byte_budget() {
        let source = RgbaImage::from_pixel(1_080, 2_400, Rgba([20, 40, 60, 255]));

        let resized = resize_for_transport(&source).expect("resize should succeed");
        let (width, height) = resized.dimensions();
        let rgba_bytes = u64::from(width) * u64::from(height) * RGBA_BYTES_PER_PIXEL;

        assert!(width <= MAX_FRAME_WIDTH);
        assert!(height <= MAX_FRAME_HEIGHT);
        assert!(rgba_bytes <= u64::try_from(MAX_FRAME_BYTES).unwrap());
        let aspect_error = (u64::from(width) * 2_400).abs_diff(u64::from(height) * 1_080);
        assert!(aspect_error <= 2_400, "aspect error was {aspect_error}");
    }

    #[test]
    fn jpeg_round_trip_has_bounded_dimensions() {
        let source = RgbaImage::from_pixel(1_600, 900, Rgba([40, 120, 220, 255]));

        let encoded = encode_jpeg(&source, 75).expect("encoding should succeed");
        let decoded = decode_jpeg(&encoded.bytes).expect("decoding should succeed");

        assert_eq!((encoded.width, encoded.height), (1_280, 720));
        assert_eq!(decoded.dimensions(), (1_280, 720));
        assert!(encoded.bytes.len() <= MAX_FRAME_BYTES);
    }

    #[test]
    fn invalid_quality_is_rejected() {
        let source = RgbaImage::new(1, 1);

        assert!(encode_jpeg(&source, 0).is_err());
        assert!(encode_jpeg(&source, 101).is_err());
    }

    #[test]
    fn oversized_compressed_payload_is_rejected_before_decode() {
        let payload = vec![0_u8; MAX_FRAME_BYTES + 1];

        let error = decode_jpeg(&payload).expect_err("oversized payload must fail");

        assert!(error.to_string().contains("payload"));
    }

    #[test]
    fn non_jpeg_payload_is_rejected() {
        let error = decode_jpeg(b"not a jpeg").expect_err("wrong magic must fail");

        assert!(error.to_string().contains("SOI"));
    }

    #[test]
    fn oversized_advertised_width_is_rejected() {
        let payload = unchecked_jpeg(MAX_FRAME_WIDTH + 1, 1);

        let error = decode_jpeg(&payload).expect_err("wide frame must fail");

        assert!(error.to_string().contains("width"));
    }

    #[test]
    fn advertised_rgba_allocation_over_budget_is_rejected() {
        // The JPEG's RGB output is below 8 MiB, but a normal RGBA texture upload
        // would exceed it. The receiver budgets for that eventual allocation.
        let payload = unchecked_jpeg(1_280, 1_700);

        let error = decode_jpeg(&payload).expect_err("RGBA budget must be enforced");

        assert!(error.to_string().contains("RGBA"));
    }

    fn unchecked_jpeg(width: u32, height: u32) -> Vec<u8> {
        let image = RgbImage::from_pixel(width, height, Rgb([12, 34, 56]));
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 70)
            .encode(image.as_raw(), width, height, ColorType::Rgb8.into())
            .expect("test JPEG encoding should succeed");
        bytes
    }
}
