//! Decode, downscale and re-encode chapter images for the reader.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageOutputFormat};

use crate::config::{ImagesConfig, SourceKind};
use crate::error::ScrapeError;

/// What becomes of transparency when an image is re-encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alpha {
    /// A translucent image is stored as PNG so its transparency survives.
    Keep,
    /// Transparency is composited onto white and the image stored as JPEG.
    /// Manga pages are opaque in substance; the alpha channel some sites
    /// leave on them only made the stored PNG several times larger.
    FlattenToWhite,
}

impl Alpha {
    pub fn for_kind(kind: SourceKind) -> Self {
        match kind {
            SourceKind::Text => Alpha::Keep,
            SourceKind::MangaTcb | SourceKind::MangaWordpress => Alpha::FlattenToWhite,
        }
    }
}

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

/// Composite onto white, so a pixel's transparency becomes lightness.
fn flatten_onto_white(img: &DynamicImage) -> image::RgbImage {
    let rgba = img.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = p.0[3] as u32;
        let blend = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        rgb.put_pixel(x, y, image::Rgb([blend(p.0[0]), blend(p.0[1]), blend(p.0[2])]));
    }
    rgb
}

/// Decode `bytes`, downscale to fit `limits`, and re-encode as JPEG, or as
/// PNG when the image has real transparency and `alpha` keeps it. A JPEG
/// that already fits is returned unchanged, and a PNG that already fits is
/// kept whenever it is smaller than its re-encode: a site's optimised
/// palette PNG can be a third the size of the JPEG it would become. Every
/// other format is re-encoded. Animated formats keep their first frame.
pub fn process_image(bytes: &[u8], limits: &ImagesConfig, alpha: Alpha) -> Result<ProcessedImage, ScrapeError> {
    let reader = image::io::Reader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(image_err)?;
    let format = reader.format();
    let img = reader.decode().map_err(image_err)?;
    let (w, h) = (img.width(), img.height());
    let fits = w <= limits.max_width && h <= limits.max_height;

    if fits && format == Some(ImageFormat::Jpeg) {
        return Ok(ProcessedImage {
            bytes: bytes.to_vec(),
            mime: "image/jpeg",
            ext: "jpg",
            width: w,
            height: h,
        });
    }

    let img = if fits {
        img
    } else {
        let (tw, th) = fit_dimensions(w, h, limits);
        img.resize_exact(tw, th, FilterType::Lanczos3)
    };
    let (width, height) = (img.width(), img.height());

    let mut out = Vec::new();
    let (mime, ext) = if alpha == Alpha::Keep && has_translucent_pixel(&img) {
        DynamicImage::ImageRgba8(img.to_rgba8())
            .write_to(&mut Cursor::new(&mut out), ImageOutputFormat::Png)
            .map_err(image_err)?;
        ("image/png", "png")
    } else {
        let rgb = if img.color().has_alpha() { flatten_onto_white(&img) } else { img.to_rgb8() };
        JpegEncoder::new_with_quality(&mut out, limits.jpeg_quality)
            .encode_image(&rgb)
            .map_err(image_err)?;
        ("image/jpeg", "jpg")
    };

    if fits && format == Some(ImageFormat::Png) && bytes.len() < out.len() {
        return Ok(ProcessedImage {
            bytes: bytes.to_vec(),
            mime: "image/png",
            ext: "png",
            width,
            height,
        });
    }

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
        let out = process_image(&png(3000, 4000, false), &limits(), Alpha::Keep).unwrap();
        assert_eq!((out.mime, out.ext), ("image/jpeg", "jpg"));
        assert_eq!((out.width, out.height), (1260, 1680));
        assert_eq!(image::guess_format(&out.bytes).unwrap(), ImageFormat::Jpeg);
    }

    #[test]
    fn oversized_wide_image_is_bounded_by_width() {
        let out = process_image(&jpeg(6000, 2000), &limits(), Alpha::Keep).unwrap();
        assert_eq!((out.width, out.height), (2528, 843));
    }

    #[test]
    fn oversized_png_with_alpha_stays_png() {
        let out = process_image(&png(100, 4000, true), &limits(), Alpha::Keep).unwrap();
        assert_eq!((out.mime, out.ext), ("image/png", "png"));
        assert_eq!(out.height, 1680);
    }

    #[test]
    fn only_a_fitting_jpeg_passes_through_unchanged() {
        let j = jpeg(800, 1200);
        let out = process_image(&j, &limits(), Alpha::Keep).unwrap();
        assert_eq!(out.bytes, j);
        assert_eq!((out.mime, out.width, out.height), ("image/jpeg", 800, 1200));
    }

    /// A 64x64 PNG of deterministic noise: PNG compresses it poorly, so the
    /// JPEG re-encode is the smaller of the two.
    fn png_noisy(alpha: bool) -> Vec<u8> {
        let mut state: u32 = 12345;
        let mut next = move || {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 16) as u8
        };
        let mut out = Vec::new();
        if alpha {
            let img = ImageBuffer::from_fn(64, 64, |_, _| Rgba([next(), next(), next(), 128]));
            PngEncoder::new(&mut out)
                .write_image(img.as_raw(), 64, 64, image::ColorType::Rgba8)
                .unwrap();
        } else {
            let img = ImageBuffer::from_fn(64, 64, |_, _| Rgb([next(), next(), next()]));
            PngEncoder::new(&mut out)
                .write_image(img.as_raw(), 64, 64, image::ColorType::Rgb8)
                .unwrap();
        }
        out
    }

    #[test]
    fn a_fitting_opaque_png_becomes_jpeg_when_that_is_smaller() {
        let p = png_noisy(false);
        let out = process_image(&p, &limits(), Alpha::Keep).unwrap();
        assert_eq!((out.mime, out.ext, out.width, out.height), ("image/jpeg", "jpg", 64, 64));
        assert!(out.bytes.len() < p.len(), "{} vs {}", out.bytes.len(), p.len());
    }

    /// A solid-colour PNG is a few hundred bytes; its JPEG would be larger.
    #[test]
    fn a_fitting_png_smaller_than_its_re_encode_is_kept_as_is() {
        for (alpha_pixels, policy) in [(false, Alpha::Keep), (true, Alpha::Keep), (false, Alpha::FlattenToWhite), (true, Alpha::FlattenToWhite)] {
            let p = png(500, 700, alpha_pixels);
            let out = process_image(&p, &limits(), policy).unwrap();
            assert_eq!(out.bytes, p, "{:?} {:?}", alpha_pixels, policy);
            assert_eq!((out.mime, out.ext, out.width, out.height), ("image/png", "png", 500, 700));
        }
    }

    #[test]
    fn a_fitting_translucent_png_is_re_encoded_as_png_when_that_is_smaller() {
        let p = png_noisy(true);
        let out = process_image(&p, &limits(), Alpha::Keep).unwrap();
        assert_eq!(out.mime, "image/png");
        assert_eq!(image::guess_format(&out.bytes).unwrap(), image::ImageFormat::Png);
    }

    /// Manga pages drop their alpha channel: composited onto white, stored
    /// as JPEG, whether or not they fit.
    #[test]
    fn flatten_composites_a_translucent_png_onto_white_as_jpeg() {
        let p = png_noisy(true);
        let out = process_image(&p, &limits(), Alpha::FlattenToWhite).unwrap();
        assert_eq!((out.mime, out.ext), ("image/jpeg", "jpg"));
        let decoded = image::load_from_memory(&out.bytes).unwrap().to_rgb8();
        // Half-transparent noise over white averages near (191, 191, 191):
        // every channel is (c * 128 + 255 * 127) / 255 for c uniform in 0..=255.
        let mean = decoded.pixels().map(|p| p.0[0] as u64).sum::<u64>() / (64 * 64);
        assert!((180..=200).contains(&mean), "mean red {} is not lightened by white", mean);

        let tall = process_image(&png(100, 4000, true), &limits(), Alpha::FlattenToWhite).unwrap();
        assert_eq!((tall.mime, tall.height), ("image/jpeg", 1680));
        let decoded = image::load_from_memory(&tall.bytes).unwrap().to_rgb8();
        let p = decoded.get_pixel(20, 800).0;
        // (10, 120, 200) at alpha 128 over white, within JPEG error
        assert!((p[0] as i32 - 132).abs() <= 4 && (p[1] as i32 - 187).abs() <= 4 && (p[2] as i32 - 227).abs() <= 4, "{:?}", p);
    }

    #[test]
    fn alpha_policy_follows_the_source_kind() {
        assert_eq!(Alpha::for_kind(SourceKind::Text), Alpha::Keep);
        assert_eq!(Alpha::for_kind(SourceKind::MangaTcb), Alpha::FlattenToWhite);
        assert_eq!(Alpha::for_kind(SourceKind::MangaWordpress), Alpha::FlattenToWhite);
    }

    #[test]
    fn small_images_are_never_scaled_up() {
        let out = process_image(&jpeg(300, 400), &limits(), Alpha::Keep).unwrap();
        assert_eq!((out.width, out.height), (300, 400));
    }

    #[test]
    fn gif_is_stored_as_its_first_frame() {
        let out = process_image(&gif(40, 30), &limits(), Alpha::Keep).unwrap();
        assert!(matches!(out.mime, "image/jpeg" | "image/png"));
        assert_eq!((out.width, out.height), (40, 30));
    }

    #[test]
    fn webp_decodes_lossless_and_lossy() {
        for bytes in [
            include_bytes!("../tests/fixtures/images/tiny-lossless.webp").as_slice(),
            include_bytes!("../tests/fixtures/images/tiny-lossy.webp").as_slice(),
        ] {
            let out = process_image(bytes, &limits(), Alpha::Keep).unwrap();
            assert_eq!((out.width, out.height), (6, 4));
            assert_eq!(out.mime, "image/jpeg");
        }
    }

    #[test]
    fn non_image_bytes_are_an_error() {
        let err = process_image(b"<html>not found</html>", &limits(), Alpha::Keep).unwrap_err();
        assert!(matches!(err, ScrapeError::Image(_)));
    }
}
