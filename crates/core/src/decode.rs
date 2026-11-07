use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use image::{DynamicImage, ImageDecoder, ImageReader};

use crate::format::{self, InputFormat};

pub struct Decoded {
    pub image: DynamicImage,
    pub icc_profile: Option<Vec<u8>>,
    pub format: InputFormat,
}

/// Decodes an image file that has already been read into memory.
pub fn decode(path: &Path, bytes: &[u8]) -> Result<Decoded> {
    let format = format::detect(path, bytes).ok_or_else(|| anyhow!("unrecognized image format"))?;
    let (image, icc_profile) = match format {
        InputFormat::Avif => (decode_avif(bytes)?, None),
        InputFormat::Jxl => decode_jxl(bytes)?,
        InputFormat::Svg => (decode_svg(path, bytes)?, None),
        InputFormat::Raw => (decode_raw(path)?, None),
        InputFormat::Heif => decode_heif(bytes)?,
        InputFormat::Tga => decode_with_image(bytes, Some(image::ImageFormat::Tga))?,
        _ => decode_with_image(bytes, None)?,
    };
    Ok(Decoded { image, icc_profile, format })
}

fn finish_decoder(mut decoder: impl ImageDecoder) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    let orientation = decoder.orientation()?;
    let icc = decoder.icc_profile().ok().flatten();
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok((image, icc))
}

fn decode_with_image(
    bytes: &[u8],
    format: Option<image::ImageFormat>,
) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    let mut reader = match format {
        Some(f) => ImageReader::with_format(Cursor::new(bytes), f),
        None => ImageReader::new(Cursor::new(bytes)).with_guessed_format()?,
    };
    reader.no_limits();
    finish_decoder(reader.into_decoder()?)
}

#[cfg(feature = "jxl")]
fn decode_jxl(bytes: &[u8]) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    finish_decoder(jxl_oxide::integration::JxlDecoder::new(Cursor::new(bytes))?)
}

#[cfg(not(feature = "jxl"))]
fn decode_jxl(_: &[u8]) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    anyhow::bail!("JPEG XL support is disabled in this build")
}

#[cfg(feature = "avif-decode")]
fn decode_avif(bytes: &[u8]) -> Result<DynamicImage> {
    use avif_decode::{Decoder, Image};
    use image::ImageBuffer;

    fn contiguous<P: Copy>(img: imgref::ImgVec<P>) -> (Vec<P>, u32, u32) {
        let (buf, w, h) = img.into_contiguous_buf();
        (buf, w as u32, h as u32)
    }
    let err = || anyhow!("invalid AVIF dimensions");

    let image = Decoder::from_avif(bytes)?.to_image()?;
    let decoded = match image {
        Image::Rgb8(img) => {
            let (px, w, h) = contiguous(img);
            DynamicImage::ImageRgb8(ImageBuffer::from_raw(w, h, bytemuck::cast_slice(&px).to_vec()).ok_or_else(err)?)
        }
        Image::Rgba8(img) => {
            let (px, w, h) = contiguous(img);
            DynamicImage::ImageRgba8(ImageBuffer::from_raw(w, h, bytemuck::cast_slice(&px).to_vec()).ok_or_else(err)?)
        }
        Image::Rgb16(img) => {
            let (px, w, h) = contiguous(img);
            DynamicImage::ImageRgb16(ImageBuffer::from_raw(w, h, bytemuck::cast_slice(&px).to_vec()).ok_or_else(err)?)
        }
        Image::Rgba16(img) => {
            let (px, w, h) = contiguous(img);
            DynamicImage::ImageRgba16(ImageBuffer::from_raw(w, h, bytemuck::cast_slice(&px).to_vec()).ok_or_else(err)?)
        }
        Image::Gray8(img) => {
            let (px, w, h) = contiguous(img);
            let data = px.into_iter().map(|g| g.value()).collect();
            DynamicImage::ImageLuma8(ImageBuffer::from_raw(w, h, data).ok_or_else(err)?)
        }
        Image::Gray16(img) => {
            let (px, w, h) = contiguous(img);
            let data = px.into_iter().map(|g| g.value()).collect();
            DynamicImage::ImageLuma16(ImageBuffer::from_raw(w, h, data).ok_or_else(err)?)
        }
    };
    // Encoders store 8-bit sources as 10-bit AV1 for precision; treat them as 8-bit images.
    Ok(match decoded {
        DynamicImage::ImageRgb16(_) => DynamicImage::ImageRgb8(decoded.to_rgb8()),
        DynamicImage::ImageRgba16(_) => DynamicImage::ImageRgba8(decoded.to_rgba8()),
        DynamicImage::ImageLuma16(_) => DynamicImage::ImageLuma8(decoded.to_luma8()),
        other => other,
    })
}

#[cfg(not(feature = "avif-decode"))]
fn decode_avif(_: &[u8]) -> Result<DynamicImage> {
    anyhow::bail!("AVIF input support is disabled in this build")
}

#[cfg(feature = "heif")]
fn decode_heif(bytes: &[u8]) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    use heic::{DecoderConfig, ImageInfo, PixelLayout};

    let info = ImageInfo::from_bytes(bytes).map_err(|e| anyhow!("invalid HEIF file: {e}"))?;
    let layout = if info.has_alpha { PixelLayout::Rgba8 } else { PixelLayout::Rgb8 };
    let decoder = DecoderConfig::new();
    // Rotation and mirroring from the container are applied by the decoder.
    let out = decoder
        .decode_request(bytes)
        .with_output_layout(layout)
        .decode()
        .map_err(|e| anyhow!("HEIF decoding failed: {e}"))?;

    // iPhones usually embed a Display P3 ICC profile; some files only signal P3 via CICP.
    const CICP_DISPLAY_P3: u16 = 12;
    let icc = decoder.extract_icc(bytes).ok().flatten().or_else(|| {
        (info.color_primaries == CICP_DISPLAY_P3)
            .then(|| moxcms::ColorProfile::new_display_p3().encode().ok())
            .flatten()
    });

    let err = || anyhow!("invalid HEIF dimensions");
    let image = if info.has_alpha {
        DynamicImage::ImageRgba8(image::RgbaImage::from_raw(out.width, out.height, out.data).ok_or_else(err)?)
    } else {
        DynamicImage::ImageRgb8(image::RgbImage::from_raw(out.width, out.height, out.data).ok_or_else(err)?)
    };
    Ok((image, icc))
}

#[cfg(not(feature = "heif"))]
fn decode_heif(_: &[u8]) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    anyhow::bail!("HEIC/HEIF support is disabled in this build")
}

#[cfg(feature = "svg")]
fn decode_svg(path: &Path, bytes: &[u8]) -> Result<DynamicImage> {
    use resvg::{tiny_skia, usvg};
    use std::sync::{Arc, OnceLock};

    // Loading system fonts is slow, so share one database across all files.
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let fontdb = FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone();

    let options = usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        fontdb,
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).context("invalid SVG")?;
    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or_else(|| anyhow!("SVG has zero size"))?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());

    let data = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    let buf = image::RgbaImage::from_raw(size.width(), size.height(), data)
        .ok_or_else(|| anyhow!("invalid SVG dimensions"))?;
    Ok(DynamicImage::ImageRgba8(buf))
}

#[cfg(not(feature = "svg"))]
fn decode_svg(_: &Path, _: &[u8]) -> Result<DynamicImage> {
    anyhow::bail!("SVG support is disabled in this build")
}

#[cfg(feature = "raw")]
fn decode_raw(path: &Path) -> Result<DynamicImage> {
    use rawler::imgop::develop::RawDevelop;

    let raw = rawler::decode_file(path).context("unsupported or damaged RAW file")?;
    let developed = RawDevelop::default()
        .develop_intermediate(&raw)
        .context("failed to develop RAW image")?;
    developed
        .to_dynamic_image()
        .ok_or_else(|| anyhow!("failed to convert developed RAW image"))
}

#[cfg(not(feature = "raw"))]
fn decode_raw(_: &Path) -> Result<DynamicImage> {
    anyhow::bail!("Camera RAW support is disabled in this build")
}
