use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::decode;
use crate::encode::{self, Effort, EncodeSettings};
use crate::format::{self, InputFormat, OutputFormat};
use crate::transform::{self, Resize};

#[derive(Debug, Clone, PartialEq)]
pub struct ConvertOptions {
    /// `None` keeps the input format (input-only formats fall back to JPEG/PNG).
    pub format: Option<OutputFormat>,
    pub quality: u8,
    pub lossless: bool,
    pub effort: Effort,
    pub resize: Option<Resize>,
    /// Pick the highest quality (up to `quality`) whose output fits in this many bytes.
    pub target_size: Option<u64>,
    /// `None` writes next to the input file.
    pub output_dir: Option<PathBuf>,
    /// Replace existing files (including the input itself) instead of picking a new name.
    pub overwrite: bool,
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
        }
    }
}

#[derive(Debug, Clone)]
pub struct Report {
    pub input: PathBuf,
    pub output: PathBuf,
    pub input_format: InputFormat,
    pub output_format: OutputFormat,
    pub input_size: u64,
    pub output_size: u64,
    pub width: u32,
    pub height: u32,
    /// Quality picked by the target-size search.
    pub quality_used: Option<u8>,
    /// Re-encoding didn't make the file smaller, so the original bytes were written.
    pub kept_original: bool,
    /// Even the lowest quality didn't fit the target size.
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

pub fn convert_file(input: &Path, opts: &ConvertOptions) -> Result<Report> {
    let start = Instant::now();
    let bytes = fs::read(input).with_context(|| format!("can't read {}", input.display()))?;
    let input_size = bytes.len() as u64;
    let decoded = decode::decode(input, &bytes)?;
    let output_format = opts.format.unwrap_or_else(|| decoded.format.default_output());

    let mut image = decoded.image;
    if let Some(icc) = &decoded.icc_profile {
        image = transform::to_srgb(image, icc);
    }
    if let Some(resize) = &opts.resize {
        image = transform::resize(image, resize)?;
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
            let data = encode::encode(&image, &settings)?;
            let missed = data.len() as u64 > target;
            (data, None, missed)
        }
        None => (encode::encode(&image, &settings)?, None, false),
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

    let output = write_output(input, output_format, opts, &data)?;
    Ok(Report {
        input: input.to_path_buf(),
        output,
        input_format: decoded.format,
        output_format,
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
        let data = encode::encode(image, &EncodeSettings { quality: q, ..settings })?;
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

fn write_output(input: &Path, format: OutputFormat, opts: &ConvertOptions, data: &[u8]) -> Result<PathBuf> {
    let dir = match &opts.output_dir {
        Some(dir) => {
            fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
            dir.clone()
        }
        None => input.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
    let ext = format.extension();
    let first = dir.join(format!("{stem}.{ext}"));

    if opts.overwrite {
        // Write to a temp file first so a failure never leaves a truncated original.
        let tmp = dir.join(format!(".{stem}.{ext}.tmp"));
        fs::write(&tmp, data).with_context(|| format!("can't write {}", tmp.display()))?;
        fs::rename(&tmp, &first).with_context(|| format!("can't write {}", first.display()))?;
        return Ok(first);
    }

    // Claim a free name atomically so parallel jobs never write the same file.
    let candidates = std::iter::once(first.clone())
        .filter(|p| !same_file(p, input))
        .chain((1..).map(|i| {
            let suffix = if i == 1 { "-min".to_string() } else { format!("-min-{i}") };
            dir.join(format!("{stem}{suffix}.{ext}"))
        }));
    for path in candidates.take(10_000) {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(data) {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(e).with_context(|| format!("can't write {}", path.display()));
                }
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("can't write {}", path.display())),
        }
    }
    anyhow::bail!("no free output file name in {}", dir.display())
}

/// Expands folders into supported image files (recursively if asked), keeping files as given.
pub fn collect_inputs(paths: &[PathBuf], recursive: bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            let walker = walkdir::WalkDir::new(path).max_depth(if recursive { usize::MAX } else { 1 });
            files.extend(
                walker
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file() && format::is_supported_path(e.path()))
                    .map(|e| e.into_path()),
            );
        } else {
            files.push(path.clone());
        }
    }
    files
}

/// Converts files in parallel. `on_done` is called from worker threads with the
/// index of each finished input. Files not started yet are skipped once `cancel` is set.
pub fn convert_batch<F>(inputs: &[PathBuf], opts: &ConvertOptions, cancel: &AtomicBool, on_done: F)
where
    F: Fn(usize, Result<Report>) + Sync,
{
    inputs.par_iter().enumerate().for_each(|(i, input)| {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        on_done(i, convert_file(input, opts));
    });
}
