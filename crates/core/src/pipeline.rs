use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use rayon::prelude::*;

use crate::decode;
use crate::encode::{self, Effort, EncodeSettings};
use crate::error::{ErrorKind, PressaError, ResultExt};
use crate::format::{self, OutputFormat};
use crate::transform::{self, Resize};
use crate::video::{self, VideoOptions};

/// Suffix of the folder that mirrors an added folder when saving next to the originals.
pub const MIRROR_SUFFIX: &str = "-pressa";

#[derive(Debug, Clone, PartialEq)]
pub struct ConvertOptions {
    /// Image output format; `None` keeps the input format (input-only formats fall back to JPEG/PNG).
    pub format: Option<OutputFormat>,
    pub quality: u8,
    pub lossless: bool,
    pub effort: Effort,
    pub resize: Option<Resize>,
    /// Images: highest quality (up to `quality`) that fits. Videos: average bitrate that fits.
    pub target_size: Option<u64>,
    /// `None` writes next to the input (folders are mirrored into a `<name>-pressa` sibling).
    pub output_dir: Option<PathBuf>,
    /// Replace existing files (including the input itself) instead of picking a new name.
    pub overwrite: bool,
    pub video: VideoOptions,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            format: None,
            quality: 80,
            lossless: false,
            effort: Effort::Balanced,
            resize: None,
            target_size: None,
            output_dir: None,
            overwrite: false,
            video: VideoOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

/// A file to convert. `root` is the folder the user added it through, so the output
/// can reproduce the subfolder structure.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InputFile {
    pub path: PathBuf,
    pub root: Option<PathBuf>,
}

impl InputFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), root: None }
    }

    pub fn kind(&self) -> MediaKind {
        if video::is_video_path(&self.path) { MediaKind::Video } else { MediaKind::Image }
    }

    /// Path relative to the added folder, or just the file name.
    pub fn display_path(&self) -> PathBuf {
        self.root
            .as_ref()
            .and_then(|root| self.path.strip_prefix(root.parent().unwrap_or(root)).ok())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.path.file_name().map(PathBuf::from).unwrap_or_default())
    }
}

#[derive(Debug, Clone)]
pub struct Report {
    pub input: PathBuf,
    pub output: PathBuf,
    pub kind: MediaKind,
    pub input_format: String,
    /// Output format, plus codec and encoder for videos (e.g. "MP4 · H.265 (HEVC) · NVENC").
    pub output_format: String,
    pub input_size: u64,
    pub output_size: u64,
    pub width: u32,
    pub height: u32,
    /// Quality picked by the target-size search.
    pub quality_used: Option<u8>,
    /// Re-encoding didn't make the file smaller, so the original was copied.
    pub kept_original: bool,
    /// The output is still larger than the target size.
    pub target_missed: bool,
    pub elapsed: Duration,
}

impl Report {
    /// Size change in percent; negative means smaller.
    pub fn change_percent(&self) -> f64 {
        if self.input_size == 0 {
            return 0.0;
        }
        (self.output_size as f64 / self.input_size as f64 - 1.0) * 100.0
    }
}

/// Folder the output of `input` goes to.
pub fn output_dir(input: &InputFile, opts: &ConvertOptions) -> PathBuf {
    let parent = input.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let Some(root) = &input.root else {
        return opts.output_dir.clone().unwrap_or(parent);
    };
    let subfolders = parent.strip_prefix(root).map(Path::to_path_buf).unwrap_or_default();
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "pressa".into());
    match &opts.output_dir {
        Some(out) => out.join(name).join(subfolders),
        None if opts.overwrite => parent,
        None => root.parent().unwrap_or(root).join(format!("{name}{MIRROR_SUFFIX}")).join(subfolders),
    }
}

pub fn convert_file(
    input: &InputFile,
    opts: &ConvertOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f32),
) -> Result<Report> {
    match input.kind() {
        MediaKind::Image => convert_image(input, opts),
        MediaKind::Video => convert_video(input, opts, cancel, progress),
    }
}

fn convert_image(input: &InputFile, opts: &ConvertOptions) -> Result<Report> {
    let start = Instant::now();
    let path = &input.path;
    let bytes = fs::read(path).kind(ErrorKind::ReadFailed)?;
    let input_size = bytes.len() as u64;
    if format::detect(path, &bytes).is_none() {
        return Err(PressaError::new(ErrorKind::UnsupportedFormat, "unrecognized image format").into());
    }
    let decoded = decode::decode(path, &bytes).kind(ErrorKind::DecodeFailed)?;
    let output_format = opts.format.unwrap_or_else(|| decoded.format.default_output());

    let mut image = decoded.image;
    if let Some(icc) = &decoded.icc_profile {
        image = transform::to_srgb(image, icc);
    }
    if let Some(resize) = &opts.resize {
        image = transform::resize(image, resize).kind(ErrorKind::EncodeFailed)?;
    }

    let settings = EncodeSettings {
        format: output_format,
        quality: opts.quality,
        lossless: opts.lossless,
        effort: opts.effort,
    };
    let searchable = output_format.is_lossy() && !(opts.lossless && output_format.supports_lossless());
    let (mut data, quality_used, mut target_missed) = match opts.target_size {
        Some(target) if searchable => encode_to_target(&image, settings, target)?,
        Some(target) => {
            let data = encode::encode(&image, &settings).kind(ErrorKind::EncodeFailed)?;
            let missed = data.len() as u64 > target;
            (data, None, missed)
        }
        None => (encode::encode(&image, &settings).kind(ErrorKind::EncodeFailed)?, None, false),
    };

    // Re-compressing an already well-compressed file can make it bigger; keep the original then.
    let same_format = decoded.format.as_output() == Some(output_format);
    let kept_original = same_format && opts.resize.is_none() && data.len() >= bytes.len();
    if kept_original {
        data = bytes;
        if let Some(target) = opts.target_size {
            target_missed = data.len() as u64 > target;
        }
    }

    let output = write_output(input, opts, output_format.extension(), |tmp| {
        fs::write(tmp, &data).kind(ErrorKind::WriteFailed)
    })?;
    Ok(Report {
        input: path.clone(),
        output,
        kind: MediaKind::Image,
        input_format: decoded.format.name().to_string(),
        output_format: output_format.name().to_string(),
        input_size,
        output_size: data.len() as u64,
        width: image.width(),
        height: image.height(),
        quality_used,
        kept_original,
        target_missed,
        elapsed: start.elapsed(),
    })
}

fn convert_video(
    input: &InputFile,
    opts: &ConvertOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f32),
) -> Result<Report> {
    let start = Instant::now();
    let path = &input.path;
    let input_size = fs::metadata(path).kind(ErrorKind::ReadFailed)?.len();
    let container = video::output_extension(path);
    let input_ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let same_container = matches!((input_ext.as_str(), container), ("mp4" | "m4v", "mp4") | ("mkv", "mkv"));

    let mut result = None;
    let output = write_output(input, opts, container, |tmp| {
        let job = video::EncodeJob {
            input: path,
            output: tmp,
            container,
            opts: &opts.video,
            effort: opts.effort,
            resize: opts.resize,
            target_size: opts.target_size,
        };
        let encoded = video::encode(&job, cancel, progress)?;
        let mut size = fs::metadata(tmp).kind(ErrorKind::WriteFailed)?.len();
        let kept = same_container && opts.resize.is_none() && size >= input_size;
        if kept {
            fs::copy(path, tmp).kind(ErrorKind::WriteFailed)?;
            size = input_size;
        }
        result = Some((encoded, size, kept));
        Ok(())
    })?;
    let (encoded, output_size, kept_original) = result.expect("set on success");

    let (width, height) = match opts.resize {
        Some(r) => r.target_size(encoded.probe.width, encoded.probe.height),
        None => (encoded.probe.width, encoded.probe.height),
    };
    Ok(Report {
        input: path.clone(),
        output,
        kind: MediaKind::Video,
        input_format: format!("{} · {}", input_ext.to_ascii_uppercase(), encoded.probe.video_codec),
        output_format: if kept_original {
            input_ext.to_ascii_uppercase()
        } else {
            format!(
                "{} · {} · {}",
                container.to_ascii_uppercase(),
                opts.video.codec.name(),
                video::encoder_label(encoded.encoder)
            )
        },
        input_size,
        output_size,
        width,
        height,
        quality_used: None,
        kept_original,
        target_missed: opts.target_size.is_some_and(|t| output_size > t),
        elapsed: start.elapsed(),
    })
}

/// Binary search for the highest quality ≤ `settings.quality` that fits `target` bytes.
fn encode_to_target(
    image: &image::DynamicImage,
    settings: EncodeSettings,
    target: u64,
) -> Result<(Vec<u8>, Option<u8>, bool)> {
    let (mut lo, mut hi) = (1u8, settings.quality.clamp(1, 100));
    let mut best: Option<(Vec<u8>, u8)> = None;
    let mut smallest: Option<(Vec<u8>, u8)> = None;
    while lo <= hi {
        let q = lo + (hi - lo) / 2;
        let data = encode::encode(image, &EncodeSettings { quality: q, ..settings }).kind(ErrorKind::EncodeFailed)?;
        if data.len() as u64 <= target {
            best = Some((data, q));
            lo = q + 1;
        } else {
            if smallest.as_ref().is_none_or(|(s, _)| data.len() < s.len()) {
                smallest = Some((data, q));
            }
            if q == 1 {
                break;
            }
            hi = q - 1;
        }
    }
    Ok(match (best, smallest) {
        (Some((data, q)), _) => (data, Some(q), false),
        (None, Some((data, q))) => (data, Some(q), true),
        (None, None) => unreachable!("search always encodes at least once"),
    })
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Picks the output path, lets `write` fill a temp file next to it, then moves it into place,
/// so a failure never leaves a truncated file behind.
fn write_output(
    input: &InputFile,
    opts: &ConvertOptions,
    ext: &str,
    write: impl FnOnce(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let dir = output_dir(input, opts);
    fs::create_dir_all(&dir).kind(ErrorKind::WriteFailed)?;
    let stem = input.path.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
    let first = dir.join(format!("{stem}.{ext}"));

    let (target, placeholder) = if opts.overwrite {
        (first, false)
    } else {
        (claim_free_name(&dir, stem, ext, &first, &input.path)?, true)
    };
    let tmp = dir.join(format!(
        ".{}.{ext}.part",
        target.file_stem().and_then(|s| s.to_str()).unwrap_or(stem)
    ));
    let result = write(&tmp).and_then(|()| fs::rename(&tmp, &target).kind(ErrorKind::WriteFailed));
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        if placeholder {
            let _ = fs::remove_file(&target);
        }
        return Err(e);
    }
    Ok(target)
}

/// Creates an empty file under a free name, so parallel jobs never pick the same one.
fn claim_free_name(dir: &Path, stem: &str, ext: &str, first: &Path, input: &Path) -> Result<PathBuf> {
    let candidates = std::iter::once(first.to_path_buf())
        .filter(|p| !same_file(p, input))
        .chain((1..).map(|i| {
            let suffix = if i == 1 { "-min".to_string() } else { format!("-min-{i}") };
            dir.join(format!("{stem}{suffix}.{ext}"))
        }));
    for path in candidates.take(10_000) {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).kind(ErrorKind::WriteFailed),
        }
    }
    Err(PressaError::new(ErrorKind::WriteFailed, format!("no free file name in {}", dir.display())).into())
}

/// True for files Pressa can convert.
pub fn is_supported(path: &Path) -> bool {
    format::is_supported_path(path) || video::is_video_path(path)
}

/// Expands folders into supported files (recursively if asked), keeping files as given.
/// Previous output folders (`*-pressa`) are skipped.
pub fn collect_inputs(paths: &[PathBuf], recursive: bool) -> Vec<InputFile> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            let walker = walkdir::WalkDir::new(path)
                .max_depth(if recursive { usize::MAX } else { 1 })
                .sort_by_file_name()
                .into_iter()
                .filter_entry(|e| {
                    e.depth() == 0
                        || !e.file_type().is_dir()
                        || !e.file_name().to_string_lossy().ends_with(MIRROR_SUFFIX)
                });
            files.extend(
                walker
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file() && is_supported(e.path()))
                    .map(|e| InputFile { path: e.into_path(), root: Some(path.clone()) }),
            );
        } else {
            files.push(InputFile::new(path.clone()));
        }
    }
    files
}

/// Converts files: images in parallel on all cores, videos one at a time alongside
/// (video encoders are multithreaded themselves). Callbacks run on worker threads;
/// `on_progress` reports 0–1 for videos. Files not started yet are skipped once `cancel` is set.
pub fn convert_batch<P, D>(inputs: &[InputFile], opts: &ConvertOptions, cancel: &AtomicBool, on_progress: P, on_done: D)
where
    P: Fn(usize, f32) + Sync,
    D: Fn(usize, Result<Report>) + Sync,
{
    let (videos, images): (Vec<usize>, Vec<usize>) =
        (0..inputs.len()).partition(|&i| inputs[i].kind() == MediaKind::Video);
    std::thread::scope(|s| {
        s.spawn(|| {
            for &i in &videos {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                on_done(i, convert_video(&inputs[i], opts, cancel, &|f| on_progress(i, f)));
            }
        });
        images.par_iter().for_each(|&i| {
            if !cancel.load(Ordering::Relaxed) {
                on_done(i, convert_image(&inputs[i], opts));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, root: Option<&str>) -> InputFile {
        InputFile { path: PathBuf::from(path), root: root.map(PathBuf::from) }
    }

    #[test]
    fn folders_are_mirrored() {
        let f = file("/photos/Trip/day1/a.jpg", Some("/photos/Trip"));
        let mut opts = ConvertOptions::default();
        assert_eq!(output_dir(&f, &opts), PathBuf::from("/photos/Trip-pressa/day1"));

        opts.output_dir = Some(PathBuf::from("/out"));
        assert_eq!(output_dir(&f, &opts), PathBuf::from("/out/Trip/day1"));

        let top = file("/photos/Trip/b.jpg", Some("/photos/Trip"));
        assert_eq!(output_dir(&top, &opts), PathBuf::from("/out/Trip"));

        opts.output_dir = None;
        opts.overwrite = true;
        assert_eq!(output_dir(&f, &opts), PathBuf::from("/photos/Trip/day1"));
    }

    #[test]
    fn loose_files_stay_put() {
        let f = file("/photos/a.jpg", None);
        assert_eq!(output_dir(&f, &ConvertOptions::default()), PathBuf::from("/photos"));
        assert_eq!(f.display_path(), PathBuf::from("a.jpg"));
        let nested = file("/photos/Trip/day1/a.jpg", Some("/photos/Trip"));
        assert_eq!(nested.display_path(), PathBuf::from("Trip/day1/a.jpg"));
    }
}
