use std::fmt;
use std::path::Path;

/// Formats the converter can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputFormat {
    Jpeg,
    Png,
    Webp,
    Avif,
    Gif,
    Bmp,
    Tiff,
    Ico,
    Tga,
    Qoi,
}

impl OutputFormat {
    pub const ALL: [OutputFormat; 10] = [
        Self::Jpeg,
        Self::Png,
        Self::Webp,
        Self::Avif,
        Self::Gif,
        Self::Bmp,
        Self::Tiff,
        Self::Ico,
        Self::Tga,
        Self::Qoi,
    ];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Gif => "gif",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::Ico => "ico",
            Self::Tga => "tga",
            Self::Qoi => "qoi",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
            Self::Avif => "AVIF",
            Self::Gif => "GIF",
            Self::Bmp => "BMP",
            Self::Tiff => "TIFF",
            Self::Ico => "ICO",
            Self::Tga => "TGA",
            Self::Qoi => "QOI",
        }
    }

    /// Whether the quality setting affects the output.
    pub fn is_lossy(self) -> bool {
        matches!(self, Self::Jpeg | Self::Webp | Self::Avif | Self::Png | Self::Gif)
    }

    /// Whether the format can also be written without any loss.
    /// (The AV1 encoder has no true lossless mode.)
    pub fn supports_lossless(self) -> bool {
        !matches!(self, Self::Jpeg | Self::Gif | Self::Avif)
    }

    pub fn from_name(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('.').to_ascii_lowercase();
        Some(match s.as_str() {
            "jpg" | "jpeg" | "jpe" | "jfif" => Self::Jpeg,
            "png" => Self::Png,
            "webp" => Self::Webp,
            "avif" => Self::Avif,
            "gif" => Self::Gif,
            "bmp" => Self::Bmp,
            "tif" | "tiff" => Self::Tiff,
            "ico" => Self::Ico,
            "tga" => Self::Tga,
            "qoi" => Self::Qoi,
            _ => return None,
        })
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Formats the converter can read, detected from content (and extension for RAW/SVG).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFormat {
    Jpeg,
    Png,
    Webp,
    Avif,
    Gif,
    Bmp,
    Tiff,
    Ico,
    Tga,
    Qoi,
    Pnm,
    Dds,
    Exr,
    Hdr,
    Farbfeld,
    Jxl,
    Svg,
    Heif,
    Raw,
}

impl InputFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
            Self::Avif => "AVIF",
            Self::Gif => "GIF",
            Self::Bmp => "BMP",
            Self::Tiff => "TIFF",
            Self::Ico => "ICO",
            Self::Tga => "TGA",
            Self::Qoi => "QOI",
            Self::Pnm => "PNM",
            Self::Dds => "DDS",
            Self::Exr => "OpenEXR",
            Self::Hdr => "Radiance HDR",
            Self::Farbfeld => "Farbfeld",
            Self::Jxl => "JPEG XL",
            Self::Svg => "SVG",
            Self::Heif => "HEIF",
            Self::Raw => "Camera RAW",
        }
    }

    /// The output format used when the user asks to keep the original format.
    pub fn default_output(self) -> OutputFormat {
        match self {
            Self::Jpeg => OutputFormat::Jpeg,
            Self::Png => OutputFormat::Png,
            Self::Webp => OutputFormat::Webp,
            Self::Avif => OutputFormat::Avif,
            Self::Gif => OutputFormat::Gif,
            Self::Bmp => OutputFormat::Bmp,
            Self::Tiff => OutputFormat::Tiff,
            Self::Ico => OutputFormat::Ico,
            Self::Tga => OutputFormat::Tga,
            Self::Qoi => OutputFormat::Qoi,
            Self::Jxl | Self::Heif | Self::Raw => OutputFormat::Jpeg,
            Self::Pnm | Self::Dds | Self::Exr | Self::Hdr | Self::Farbfeld | Self::Svg => {
                OutputFormat::Png
            }
        }
    }

    pub fn as_output(self) -> Option<OutputFormat> {
        let out = self.default_output();
        (out.name() == self.name()).then_some(out)
    }
}

impl fmt::Display for InputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

pub const RAW_EXTENSIONS: &[&str] = &[
    "3fr", "ari", "arw", "cr2", "cr3", "crw", "dcr", "dng", "erf", "fff", "iiq", "kdc", "mef",
    "mos", "mrw", "nef", "nrw", "orf", "ori", "pef", "raf", "raw", "rw2", "rwl", "srw", "x3f",
];

const OTHER_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "webp", "avif", "gif", "bmp", "dib", "tif",
    "tiff", "ico", "tga", "qoi", "pbm", "pgm", "ppm", "pam", "pnm", "dds", "exr", "hdr", "ff",
    "jxl", "svg", "svgz", "heic", "heif", "hif",
];

fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Cheap check used when scanning folders.
pub fn is_supported_path(path: &Path) -> bool {
    extension_of(path).is_some_and(|e| {
        RAW_EXTENSIONS.contains(&e.as_str()) || OTHER_EXTENSIONS.contains(&e.as_str())
    })
}

/// Detects the input format from the file content, using the extension only where
/// content sniffing is ambiguous (RAW files are often TIFF containers).
pub fn detect(path: &Path, bytes: &[u8]) -> Option<InputFormat> {
    let ext = extension_of(path).unwrap_or_default();
    if RAW_EXTENSIONS.contains(&ext.as_str()) {
        return Some(InputFormat::Raw);
    }
    if bytes.starts_with(&[0xFF, 0x0A]) || bytes.starts_with(b"\0\0\0\x0CJXL \r\n\x87\n") {
        return Some(InputFormat::Jxl);
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        match &bytes[8..12] {
            b"avif" | b"avis" => return Some(InputFormat::Avif),
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1" => {
                return Some(InputFormat::Heif);
            }
            _ => {}
        }
    }
    if let Ok(fmt) = image::guess_format(bytes) {
        use image::ImageFormat as F;
        let detected = match fmt {
            F::Jpeg => InputFormat::Jpeg,
            F::Png => InputFormat::Png,
            F::WebP => InputFormat::Webp,
            F::Avif => InputFormat::Avif,
            F::Gif => InputFormat::Gif,
            F::Bmp => InputFormat::Bmp,
            F::Tiff => InputFormat::Tiff,
            F::Ico => InputFormat::Ico,
            F::Qoi => InputFormat::Qoi,
            F::Pnm => InputFormat::Pnm,
            F::Dds => InputFormat::Dds,
            F::OpenExr => InputFormat::Exr,
            F::Hdr => InputFormat::Hdr,
            F::Farbfeld => InputFormat::Farbfeld,
            _ => return None,
        };
        return Some(detected);
    }
    match ext.as_str() {
        "svg" | "svgz" => Some(InputFormat::Svg),
        // TGA has no magic number.
        "tga" => Some(InputFormat::Tga),
        _ => {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
            let head = head.trim_start_matches('\u{feff}').trim_start();
            (head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg")))
                .then_some(InputFormat::Svg)
        }
    }
}
