//! Decode, downscale and re-encode chapter images for the reader.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageOutputFormat};

use crate::config::ImagesConfig;
use crate::error::ScrapeError;

/// An image ready to store: JPEG or PNG, bounded by the configured size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedImage {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub ext: &'static str,
    pub width: u32,
    pub height: u32,
}

fn image_err(e: impl std::fmt::Display) -> ScrapeError {
    ScrapeError::Image(e.to_string())
}

/// True when at least one pixel is not fully opaque.
fn has_translucent_pixel(img: &DynamicImage) -> bool {
    img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p.0[3] < 255)
}

/// Dimensions that fit within the limits, keeping the aspect ratio.
fn fit_dimensions(w: u32, h: u32, limits: &ImagesConfig) -> (u32, u32) {
    let scale = (limits.max_width as f64 / w as f64).min(limits.max_height as f64 / h as f64);
    let scaled = |d: u32| ((d as f64 * scale).round() as u32).max(1);
    (scaled(w), scaled(h))
}

/// Decode `bytes`, downscale to fit `limits`, and re-encode as JPEG, or PNG
/// when the image has real transparency. JPEG and PNG inputs that already fit
/// are returned unchanged. Animated formats keep their first frame.
pub fn process_image(bytes: &[u8], limits: &ImagesConfig) -> Result<ProcessedImage, ScrapeError> {
    let reader = image::io::Reader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(image_err)?;
    let format = reader.format();
    let img = reader.decode().map_err(image_err)?;
    let (w, h) = (img.width(), img.height());
    let fits = w <= limits.max_width && h <= limits.max_height;

    if fits {
        let passthrough = match format {
            Some(ImageFormat::Jpeg) => Some(("image/jpeg", "jpg")),
            Some(ImageFormat::Png) => Some(("image/png", "png")),
            _ => None,
        };
        if let Some((mime, ext)) = passthrough {
            return Ok(ProcessedImage {
                bytes: bytes.to_vec(),
                mime,
                ext,
                width: w,
                height: h,
            });
        }
    }

    let img = if fits {
        img
    } else {
        let (tw, th) = fit_dimensions(w, h, limits);
        img.resize_exact(tw, th, FilterType::Lanczos3)
    };
    let (width, height) = (img.width(), img.height());

    let mut out = Vec::new();
    let (mime, ext) = if has_translucent_pixel(&img) {
        DynamicImage::ImageRgba8(img.to_rgba8())
            .write_to(&mut Cursor::new(&mut out), ImageOutputFormat::Png)
            .map_err(image_err)?;
        ("image/png", "png")
    } else {
        let rgb = img.to_rgb8();
        JpegEncoder::new_with_quality(&mut out, limits.jpeg_quality)
            .encode_image(&rgb)
            .map_err(image_err)?;
        ("image/jpeg", "jpg")
    };

    Ok(ProcessedImage {
        bytes: out,
        mime,
        ext,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::gif::GifEncoder;
    use image::codecs::png::PngEncoder;
    use image::{Frame, ImageBuffer, ImageEncoder, Rgb, Rgba};

    fn limits() -> ImagesConfig {
        ImagesConfig::default()
    }

    fn png(w: u32, h: u32, alpha: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if alpha {
            let img = ImageBuffer::from_pixel(w, h, Rgba([10u8, 120, 200, 128]));
            PngEncoder::new(&mut out)
                .write_image(img.as_raw(), w, h, image::ColorType::Rgba8)
                .unwrap();
        } else {
            let img = ImageBuffer::from_pixel(w, h, Rgb([10u8, 120, 200]));
            PngEncoder::new(&mut out)
                .write_image(img.as_raw(), w, h, image::ColorType::Rgb8)
                .unwrap();
        }
        out
    }

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = ImageBuffer::from_pixel(w, h, Rgb([10u8, 120, 200]));
        let mut out = Vec::new();
        JpegEncoder::new(&mut out)
            .write_image(img.as_raw(), w, h, image::ColorType::Rgb8)
            .unwrap();
        out
    }

    fn gif(w: u32, h: u32) -> Vec<u8> {
        let img = ImageBuffer::from_pixel(w, h, Rgba([10u8, 120, 200, 255]));
        let mut out = Vec::new();
        GifEncoder::new(&mut out)
            .encode_frame(Frame::new(img))
            .unwrap();
        out
    }

    #[test]
    fn oversized_rgb_png_is_scaled_to_fit_and_encoded_as_jpeg() {
        let out = process_image(&png(3000, 4000, false), &limits()).unwrap();
        assert_eq!((out.mime, out.ext), ("image/jpeg", "jpg"));
        assert_eq!((out.width, out.height), (1260, 1680));
        assert_eq!(image::guess_format(&out.bytes).unwrap(), ImageFormat::Jpeg);
    }

    #[test]
    fn oversized_wide_image_is_bounded_by_width() {
        let out = process_image(&jpeg(6000, 2000), &limits()).unwrap();
        assert_eq!((out.width, out.height), (2528, 843));
    }

    #[test]
    fn oversized_png_with_alpha_stays_png() {
        let out = process_image(&png(100, 4000, true), &limits()).unwrap();
        assert_eq!((out.mime, out.ext), ("image/png", "png"));
        assert_eq!(out.height, 1680);
    }

    #[test]
    fn jpeg_and_png_that_fit_pass_through_unchanged() {
        let j = jpeg(800, 1200);
        let out = process_image(&j, &limits()).unwrap();
        assert_eq!(out.bytes, j);
        assert_eq!((out.mime, out.width, out.height), ("image/jpeg", 800, 1200));
        let p = png(50, 50, true);
        assert_eq!(process_image(&p, &limits()).unwrap().bytes, p);
    }

    #[test]
    fn small_images_are_never_scaled_up() {
        let out = process_image(&jpeg(300, 400), &limits()).unwrap();
        assert_eq!((out.width, out.height), (300, 400));
    }

    #[test]
    fn gif_is_stored_as_its_first_frame() {
        let out = process_image(&gif(40, 30), &limits()).unwrap();
        assert!(matches!(out.mime, "image/jpeg" | "image/png"));
        assert_eq!((out.width, out.height), (40, 30));
    }

    #[test]
    fn webp_decodes_lossless_and_lossy() {
        for bytes in [
            include_bytes!("../tests/fixtures/images/tiny-lossless.webp").as_slice(),
            include_bytes!("../tests/fixtures/images/tiny-lossy.webp").as_slice(),
        ] {
            let out = process_image(bytes, &limits()).unwrap();
            assert_eq!((out.width, out.height), (6, 4));
            assert_eq!(out.mime, "image/jpeg");
        }
    }

    #[test]
    fn non_image_bytes_are_an_error() {
        let err = process_image(b"<html>not found</html>", &limits()).unwrap_err();
        assert!(matches!(err, ScrapeError::Image(_)));
    }
}
