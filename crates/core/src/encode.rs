use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};

use anyhow::{Context, Result, anyhow, bail};
use image::{DynamicImage, ImageFormat};
use rgb::FromSlice;

use crate::format::OutputFormat;
use crate::transform::{self, Resize};

/// Speed/size trade-off for encoders that support it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Effort {
    Fast,
    #[default]
    Balanced,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 3] = [Self::Fast, Self::Balanced, Self::Max];

    pub fn name(self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::Balanced => "Balanced",
            Self::Max => "Max compression",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncodeSettings {
    pub format: OutputFormat,
    /// 1–100. Ignored for lossless output.
    pub quality: u8,
    pub lossless: bool,
    pub effort: Effort,
}

pub fn encode(image: &DynamicImage, settings: &EncodeSettings) -> Result<Vec<u8>> {
    let quality = settings.quality.clamp(1, 100);
    let lossless = settings.lossless && settings.format.supports_lossless();
    match settings.format {
        OutputFormat::Jpeg => encode_jpeg(image, quality, settings.effort),
        OutputFormat::Png => encode_png(image, quality, lossless, settings.effort),
        OutputFormat::Webp => encode_webp(image, quality, lossless, settings.effort),
        OutputFormat::Avif => encode_avif(image, quality, settings.effort),
        OutputFormat::Gif => encode_gif(image, quality),
        OutputFormat::Ico => {
            let fit = Resize::Fit { max_width: Some(256), max_height: Some(256) };
            let small = transform::resize(image.clone(), &fit)?;
            // Icons embed PNGs, which the ICO spec requires to be RGBA.
            let mut out = Cursor::new(Vec::new());
            DynamicImage::ImageRgba8(small.to_rgba8()).write_to(&mut out, ImageFormat::Ico)?;
            Ok(out.into_inner())
        }
        OutputFormat::Bmp => encode_with_image(image, ImageFormat::Bmp),
        OutputFormat::Tiff => encode_tiff(image, settings.effort),
        OutputFormat::Tga => encode_with_image(image, ImageFormat::Tga),
        OutputFormat::Qoi => encode_with_image(image, ImageFormat::Qoi),
    }
}

/// RGBA8 if transparency is used, RGB8 otherwise.
fn to_8bit(image: &DynamicImage) -> DynamicImage {
    if transform::uses_alpha(image) {
        DynamicImage::ImageRgba8(image.to_rgba8())
    } else {
        DynamicImage::ImageRgb8(image.to_rgb8())
    }
}

fn encode_with_image(image: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    to_8bit(image).write_to(&mut out, format)?;
    Ok(out.into_inner())
}

fn encode_tiff(image: &DynamicImage, effort: Effort) -> Result<Vec<u8>> {
    use tiff::encoder::{Compression, DeflateLevel, Predictor, TiffEncoder, colortype};

    let level = match effort {
        Effort::Fast => DeflateLevel::Fast,
        Effort::Balanced => DeflateLevel::Balanced,
        Effort::Max => DeflateLevel::Best,
    };
    let mut out = Cursor::new(Vec::new());
    let mut encoder = TiffEncoder::new(&mut out)?
        .with_compression(Compression::Deflate(level))
        .with_predictor(Predictor::Horizontal);
    let (width, height) = (image.width(), image.height());
    match to_8bit(image) {
        DynamicImage::ImageRgba8(b) => encoder.write_image::<colortype::RGBA8>(width, height, b.as_raw())?,
        DynamicImage::ImageRgb8(b) => encoder.write_image::<colortype::RGB8>(width, height, b.as_raw())?,
        _ => unreachable!(),
    }
    Ok(out.into_inner())
}

fn encode_jpeg(image: &DynamicImage, quality: u8, effort: Effort) -> Result<Vec<u8>> {
    use mozjpeg::{ColorSpace, Compress};

    let gray = transform::is_grayscale(image) && !transform::uses_alpha(image);
    let (color_space, pixels) = if gray {
        (ColorSpace::JCS_GRAYSCALE, image.to_luma8().into_raw())
    } else if transform::uses_alpha(image) {
        (ColorSpace::JCS_RGB, transform::flatten_alpha(image).into_raw())
    } else {
        (ColorSpace::JCS_RGB, image.to_rgb8().into_raw())
    };
    let (width, height) = (image.width() as usize, image.height() as usize);

    // mozjpeg reports libjpeg errors by unwinding.
    let result = catch_unwind(AssertUnwindSafe(|| -> std::io::Result<Vec<u8>> {
        let mut c = Compress::new(color_space);
        if effort == Effort::Fast {
            c.set_fastest_defaults();
        }
        c.set_size(width, height);
        c.set_quality(quality as f32);
        if effort != Effort::Fast {
            c.set_progressive_mode();
            c.set_optimize_scans(true);
            c.set_optimize_coding(true);
        }
        if !gray && quality >= 90 {
            // Full-resolution chroma at high quality avoids color bleeding on edges.
            c.set_chroma_sampling_pixel_sizes((1, 1), (1, 1));
        }
        let mut started = c.start_compress(Vec::new())?;
        started.write_scanlines(&pixels)?;
        started.finish()
    }));
    match result {
        Ok(r) => r.context("JPEG encoding failed"),
        Err(_) => bail!("JPEG encoding failed"),
    }
}

fn oxipng_options(effort: Effort) -> oxipng::Options {
    let mut opts = oxipng::Options::from_preset(match effort {
        Effort::Fast => 1,
        Effort::Balanced => 2,
        Effort::Max => 5,
    });
    opts.strip = oxipng::StripChunks::Safe;
    opts
}

fn encode_png(image: &DynamicImage, quality: u8, lossless: bool, effort: Effort) -> Result<Vec<u8>> {
    use oxipng::{BitDepth, ColorType, RawImage};

    let (width, height) = (image.width(), image.height());
    if !lossless {
        // Lossy PNG = palette quantization (like pngquant). Fall back to lossless when
        // 256 colors can't reach the requested quality.
        if let Some((palette, indices)) = quantize(image, quality, true)? {
            let raw = RawImage::new(width, height, ColorType::Indexed { palette }, BitDepth::Eight, indices)?;
            return Ok(raw.create_optimized_png(&oxipng_options(effort))?);
        }
    }

    // oxipng reduces color type and bit depth itself, so only 8 vs 16 bits matters here.
    let alpha = transform::uses_alpha(image);
    let sixteen = matches!(
        image,
        DynamicImage::ImageLuma16(_)
            | DynamicImage::ImageLumaA16(_)
            | DynamicImage::ImageRgb16(_)
            | DynamicImage::ImageRgba16(_)
    );
    let raw = if sixteen {
        let (color, samples) = if alpha {
            (ColorType::RGBA, image.to_rgba16().into_raw())
        } else {
            (ColorType::RGB { transparent_color: None }, image.to_rgb16().into_raw())
        };
        let bytes = samples.iter().flat_map(|s| s.to_be_bytes()).collect();
        RawImage::new(width, height, color, BitDepth::Sixteen, bytes)?
    } else if alpha {
        RawImage::new(width, height, ColorType::RGBA, BitDepth::Eight, image.to_rgba8().into_raw())?
    } else {
        let color = ColorType::RGB { transparent_color: None };
        RawImage::new(width, height, color, BitDepth::Eight, image.to_rgb8().into_raw())?
    };
    Ok(raw.create_optimized_png(&oxipng_options(effort))?)
}

/// Reduces the image to at most 256 colors. Returns `None` if the quality target
/// can't be met (only when `strict`).
fn quantize(
    image: &DynamicImage,
    quality: u8,
    strict: bool,
) -> Result<Option<(Vec<rgb::RGBA8>, Vec<u8>)>> {
    let rgba = image.to_rgba8();
    let (width, height) = (rgba.width() as usize, rgba.height() as usize);
    let mut liq = imagequant::new();
    let min_quality = if strict { quality.saturating_sub(20) } else { 0 };
    liq.set_quality(min_quality, quality)?;
    liq.set_speed(4)?;
    let mut img = liq.new_image_borrowed(rgba.as_raw().as_rgba(), width, height, 0.0)?;
    let mut res = match liq.quantize(&mut img) {
        Ok(res) => res,
        Err(imagequant::Error::QualityTooLow) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    res.set_dithering_level(1.0)?;
    let (palette, indices) = res.remapped(&mut img)?;
    Ok(Some((palette, indices)))
}

fn encode_webp(image: &DynamicImage, quality: u8, lossless: bool, effort: Effort) -> Result<Vec<u8>> {
    let mut config = webp::WebPConfig::new().map_err(|_| anyhow!("WebP config error"))?;
    config.lossless = lossless as i32;
    config.quality = if lossless {
        // For lossless, quality controls compression effort.
        match effort {
            Effort::Fast => 25.0,
            Effort::Balanced => 75.0,
            Effort::Max => 100.0,
        }
    } else {
        quality as f32
    };
    config.method = match effort {
        Effort::Fast => 2,
        Effort::Balanced => 4,
        Effort::Max => 6,
    };
    config.alpha_quality = quality as i32;
    config.use_sharp_yuv = (effort == Effort::Max) as i32;

    let pixels = to_8bit(image);
    let (width, height) = (pixels.width(), pixels.height());
    let encoder = match &pixels {
        DynamicImage::ImageRgba8(b) => webp::Encoder::from_rgba(b.as_raw(), width, height),
        DynamicImage::ImageRgb8(b) => webp::Encoder::from_rgb(b.as_raw(), width, height),
        _ => unreachable!(),
    };
    let encoded = encoder
        .encode_advanced(&config)
        .map_err(|e| anyhow!("WebP encoding failed: {e:?}"))?;
    Ok(encoded.to_vec())
}

fn encode_avif(image: &DynamicImage, quality: u8, effort: Effort) -> Result<Vec<u8>> {
    let encoder = ravif::Encoder::new()
        .with_quality(quality as f32)
        .with_alpha_quality(quality as f32)
        .with_speed(match effort {
            Effort::Fast => 9,
            Effort::Balanced => 6,
            Effort::Max => 3,
        });
    let pixels = to_8bit(image);
    let (width, height) = (pixels.width() as usize, pixels.height() as usize);
    let encoded = match &pixels {
        DynamicImage::ImageRgba8(b) => {
            encoder.encode_rgba(ravif::Img::new(b.as_raw().as_rgba(), width, height))?
        }
        DynamicImage::ImageRgb8(b) => {
            encoder.encode_rgb(ravif::Img::new(b.as_raw().as_rgb(), width, height))?
        }
        _ => unreachable!(),
    };
    Ok(encoded.avif_file)
}

fn encode_gif(image: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let (Ok(width), Ok(height)) = (u16::try_from(image.width()), u16::try_from(image.height()))
    else {
        bail!("GIF supports at most 65535×65535 pixels");
    };
    let (palette, indices) = quantize(image, quality, false)?.expect("non-strict quantize");
    let transparent = palette.iter().position(|c| c.a == 0).map(|i| i as u8);
    let rgb_palette: Vec<u8> = palette.iter().flat_map(|c| [c.r, c.g, c.b]).collect();

    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, width, height, &rgb_palette)?;
        let frame = gif::Frame {
            width,
            height,
            buffer: indices.into(),
            transparent,
            ..Default::default()
        };
        encoder.write_frame(&frame)?;
    }
    Ok(out)
}
