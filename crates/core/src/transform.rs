use anyhow::Result;
use fast_image_resize::{ResizeOptions, Resizer};
use image::{ColorType, DynamicImage, ImageBuffer};
use moxcms::{ColorProfile, Layout, TransformOptions};

/// How to resize images. Images are never upscaled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Resize {
    /// Fit inside the given box, keeping the aspect ratio. `None` means unconstrained.
    Fit { max_width: Option<u32>, max_height: Option<u32> },
    /// Scale both sides by a percentage (1–100).
    Percent(f32),
}

impl Resize {
    pub fn target_size(&self, width: u32, height: u32) -> (u32, u32) {
        let scale = match *self {
            Resize::Fit { max_width, max_height } => {
                let sx = max_width.map_or(1.0, |m| m as f64 / width as f64);
                let sy = max_height.map_or(1.0, |m| m as f64 / height as f64);
                sx.min(sy)
            }
            Resize::Percent(p) => p as f64 / 100.0,
        };
        if scale >= 1.0 {
            return (width, height);
        }
        let w = ((width as f64 * scale).round() as u32).max(1);
        let h = ((height as f64 * scale).round() as u32).max(1);
        (w, h)
    }
}

/// Converts pixels from an embedded ICC profile to sRGB, since most outputs are
/// written without a profile. Images that can't be converted are returned unchanged.
pub fn to_srgb(image: DynamicImage, icc: &[u8]) -> DynamicImage {
    if image.color().channel_count() < 3 {
        return image;
    }
    let Ok(src) = ColorProfile::new_from_slice(icc) else {
        return image;
    };
    let dst = ColorProfile::new_srgb();
    let (width, height) = (image.width(), image.height());
    let has_alpha = image.color().has_alpha();
    let layout = if has_alpha { Layout::Rgba } else { Layout::Rgb };
    let Ok(transform) =
        src.create_transform_8bit(layout, &dst, layout, TransformOptions::default())
    else {
        return image;
    };

    let pixels = if has_alpha { image.to_rgba8().into_raw() } else { image.to_rgb8().into_raw() };
    let mut out = vec![0u8; pixels.len()];
    if transform.transform(&pixels, &mut out).is_err() {
        return image;
    }
    if has_alpha {
        ImageBuffer::from_raw(width, height, out).map(DynamicImage::ImageRgba8)
    } else {
        ImageBuffer::from_raw(width, height, out).map(DynamicImage::ImageRgb8)
    }
    .unwrap_or(image)
}

pub fn resize(image: DynamicImage, resize: &Resize) -> Result<DynamicImage> {
    let (width, height) = resize.target_size(image.width(), image.height());
    if (width, height) == (image.width(), image.height()) {
        return Ok(image);
    }
    // fast_image_resize works on integer pixel types; float images (EXR/HDR) go through 8-bit.
    let image = match image.color() {
        ColorType::L8
        | ColorType::La8
        | ColorType::Rgb8
        | ColorType::Rgba8
        | ColorType::L16
        | ColorType::La16
        | ColorType::Rgb16
        | ColorType::Rgba16 => image,
        _ if image.color().has_alpha() => DynamicImage::ImageRgba8(image.to_rgba8()),
        _ => DynamicImage::ImageRgb8(image.to_rgb8()),
    };
    let mut dst = DynamicImage::new(width, height, image.color());
    Resizer::new().resize(&image, &mut dst, &ResizeOptions::new())?;
    Ok(dst)
}

/// True when the image has an alpha channel that is actually used.
pub fn uses_alpha(image: &DynamicImage) -> bool {
    match image {
        DynamicImage::ImageLumaA8(b) => b.pixels().any(|p| p.0[1] != u8::MAX),
        DynamicImage::ImageRgba8(b) => b.pixels().any(|p| p.0[3] != u8::MAX),
        DynamicImage::ImageLumaA16(b) => b.pixels().any(|p| p.0[1] != u16::MAX),
        DynamicImage::ImageRgba16(b) => b.pixels().any(|p| p.0[3] != u16::MAX),
        DynamicImage::ImageRgba32F(b) => b.pixels().any(|p| p.0[3] < 1.0),
        _ => false,
    }
}

/// True when every pixel is gray (R == G == B).
pub fn is_grayscale(image: &DynamicImage) -> bool {
    match image {
        DynamicImage::ImageLuma8(_)
        | DynamicImage::ImageLumaA8(_)
        | DynamicImage::ImageLuma16(_)
        | DynamicImage::ImageLumaA16(_) => true,
        DynamicImage::ImageRgb8(b) => b.pixels().all(|p| p.0[0] == p.0[1] && p.0[1] == p.0[2]),
        DynamicImage::ImageRgba8(b) => b.pixels().all(|p| p.0[0] == p.0[1] && p.0[1] == p.0[2]),
        _ => false,
    }
}

/// Composites onto white, for formats without transparency.
pub fn flatten_alpha(image: &DynamicImage) -> image::RgbImage {
    let rgba = image.to_rgba8();
    let mut out = image::RgbImage::new(rgba.width(), rgba.height());
    for (dst, src) in out.pixels_mut().zip(rgba.pixels()) {
        let [r, g, b, a] = src.0;
        let a = a as u32;
        let blend = |c: u8| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
        dst.0 = [blend(r), blend(g), blend(b)];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_p3_converts_to_srgb() {
        let icc = ColorProfile::new_display_p3().encode().unwrap();
        let px = |r, g, b| DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1, 1, image::Rgb([r, g, b])));

        // Neutral gray is the same in both spaces.
        let gray = to_srgb(px(128, 128, 128), &icc).to_rgb8().get_pixel(0, 0).0;
        assert!(gray.iter().all(|&c| (127..=129).contains(&c)), "{gray:?}");

        // P3 has a wider gamut, so a saturated P3 orange becomes more extreme in sRGB.
        let [r, g, b] = to_srgb(px(200, 100, 50), &icc).to_rgb8().get_pixel(0, 0).0;
        assert!(r > 205 && g < 100 && b < 50, "{:?}", [r, g, b]);
    }

    #[test]
    fn fit_never_upscales() {
        let fit = Resize::Fit { max_width: Some(1000), max_height: None };
        assert_eq!(fit.target_size(4000, 3000), (1000, 750));
        assert_eq!(fit.target_size(800, 600), (800, 600));
        assert_eq!(Resize::Percent(50.0).target_size(101, 51), (51, 26));
    }
}
