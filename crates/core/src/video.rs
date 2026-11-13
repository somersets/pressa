//! Video compression by driving an FFmpeg executable.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::encode::Effort;
use crate::error::{ErrorKind, PressaError};
use crate::transform::Resize;

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "mts", "m2ts", "ts", "3gp", "mpg",
    "mpeg",
];

pub fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VideoCodec {
    H264,
    #[default]
    H265,
    Av1,
}

impl VideoCodec {
    pub const ALL: [VideoCodec; 3] = [Self::H264, Self::H265, Self::Av1];

    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::H265 => "H.265 (HEVC)",
            Self::Av1 => "AV1",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "h264" | "avc" | "x264" => Self::H264,
            "h265" | "hevc" | "x265" => Self::H265,
            "av1" => Self::Av1,
            _ => return None,
        })
    }

    fn software_encoder(self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::H265 => "libx265",
            Self::Av1 => "libsvtav1",
        }
    }

    fn hardware_encoders(self) -> &'static [&'static str] {
        if cfg!(target_os = "macos") {
            match self {
                Self::H264 => &["h264_videotoolbox"],
                Self::H265 => &["hevc_videotoolbox"],
                Self::Av1 => &[],
            }
        } else {
            match self {
                Self::H264 => &["h264_nvenc", "h264_qsv", "h264_amf"],
                Self::H265 => &["hevc_nvenc", "hevc_qsv", "hevc_amf"],
                Self::Av1 => &["av1_nvenc", "av1_qsv", "av1_amf"],
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoOptions {
    pub codec: VideoCodec,
    /// 1–100, mapped to each encoder's constant-quality scale.
    pub quality: u8,
    /// Use a GPU encoder when one is available (much faster, slightly larger files).
    pub hardware: bool,
    /// Bitrate for re-encoded audio. Compatible audio is copied as is.
    pub audio_kbps: u32,
}

impl Default for VideoOptions {
    fn default() -> Self {
        Self { codec: VideoCodec::H265, quality: 70, hardware: true, audio_kbps: 160 }
    }
}

/// Locates FFmpeg: `PRESSA_FFMPEG`, next to the executable (bundled), the dev build cache, then PATH.
pub fn ffmpeg_path() -> Option<&'static Path> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let exe_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        let mut candidates = Vec::new();
        if let Some(p) = std::env::var_os("PRESSA_FFMPEG") {
            candidates.push(PathBuf::from(p));
        }
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
            candidates.push(dir.join(exe_name));
            candidates.push(dir.join("..").join("ffmpeg").join(exe_name)); // target/release -> target/ffmpeg
        }
        candidates
            .into_iter()
            .find(|p| p.is_file())
            .or_else(|| runs(Command::new(exe_name).arg("-version")).then(|| PathBuf::from(exe_name)))
    })
    .as_deref()
}

fn command(ffmpeg: &Path) -> Command {
    let mut cmd = Command::new(ffmpeg);
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn runs(cmd: &mut Command) -> bool {
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether an encoder works on this machine (a GPU encoder needs the actual hardware).
fn encoder_works(ffmpeg: &Path, encoder: &str) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(&ok) = cache.lock().unwrap().get(encoder) {
        return ok;
    }
    let ok = runs(command(ffmpeg).args([
        "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=256x256:d=0.1",
        "-frames:v", "1", "-c:v", encoder, "-f", "null", "-",
    ]));
    cache.lock().unwrap().insert(encoder.to_string(), ok);
    ok
}

/// The encoder that will be used for these options.
pub fn pick_encoder(opts: &VideoOptions) -> Option<&'static str> {
    let ffmpeg = ffmpeg_path()?;
    let hardware = opts.hardware.then(|| {
        opts.codec.hardware_encoders().iter().copied().find(|e| encoder_works(ffmpeg, e))
    });
    Some(hardware.flatten().unwrap_or(opts.codec.software_encoder()))
}

pub fn encoder_label(encoder: &str) -> &'static str {
    match encoder.rsplit('_').next() {
        Some("nvenc") => "NVENC",
        Some("qsv") => "Quick Sync",
        Some("amf") => "AMF",
        Some("videotoolbox") => "VideoToolbox",
        _ => "CPU",
    }
}

/// What FFmpeg reports about the input.
#[derive(Debug, Default)]
pub struct Probe {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub video_codec: String,
    pub audio_codec: Option<String>,
}

pub fn probe(ffmpeg: &Path, input: &Path) -> Result<Probe, PressaError> {
    let out = command(ffmpeg)
        .args(["-hide_banner", "-i"])
        .arg(input)
        .output()
        .map_err(|e| PressaError::new(ErrorKind::VideoFailed, format!("can't run FFmpeg: {e}")))?;
    let text = String::from_utf8_lossy(&out.stderr);
    let mut p = Probe::default();
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Duration: ") {
            p.duration = parse_timestamp(rest.split(',').next().unwrap_or("")).unwrap_or(0.0);
        } else if line.starts_with("Stream #") && line.contains("Video: ") && p.video_codec.is_empty() {
            let desc = line.split("Video: ").nth(1).unwrap_or("");
            p.video_codec = first_word(desc);
            if let Some((w, h)) = desc.split(", ").find_map(parse_dimensions) {
                (p.width, p.height) = (w, h);
            }
        } else if line.starts_with("Stream #") && line.contains("Audio: ") && p.audio_codec.is_none() {
            p.audio_codec = Some(first_word(line.split("Audio: ").nth(1).unwrap_or("")));
        }
    }
    if p.video_codec.is_empty() {
        return Err(PressaError::new(ErrorKind::UnsupportedFormat, "no video stream found"));
    }
    Ok(p)
}

fn first_word(s: &str) -> String {
    s.split([' ', ',']).next().unwrap_or("").to_string()
}

fn parse_timestamp(s: &str) -> Option<f64> {
    let mut parts = s.trim().split(':');
    let (h, m, sec) = (parts.next()?, parts.next()?, parts.next()?);
    Some(h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?)
}

fn parse_dimensions(field: &str) -> Option<(u32, u32)> {
    let dims = field.split_whitespace().next()?;
    let (w, h) = dims.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Output container: MKV inputs stay MKV, everything else becomes MP4.
pub fn output_extension(input: &Path) -> &'static str {
    let ext = input.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "mkv" { "mkv" } else { "mp4" }
}

pub struct EncodeJob<'a> {
    pub input: &'a Path,
    /// Temporary file to write; the caller moves it into place.
    pub output: &'a Path,
    pub container: &'static str,
    pub opts: &'a VideoOptions,
    pub effort: Effort,
    pub resize: Option<Resize>,
    pub target_size: Option<u64>,
}

pub struct Encoded {
    pub encoder: &'static str,
    pub probe: Probe,
}

pub fn encode(
    job: &EncodeJob,
    cancel: &AtomicBool,
    progress: &dyn Fn(f32),
) -> Result<Encoded, PressaError> {
    let ffmpeg = ffmpeg_path().ok_or_else(|| PressaError::new(ErrorKind::FfmpegMissing, "FFmpeg not found"))?;
    let probe = probe(ffmpeg, job.input)?;
    let encoder = pick_encoder(job.opts).expect("FFmpeg was found");

    let mut cmd = command(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostats", "-progress", "pipe:1", "-y", "-i"])
        .arg(job.input)
        .args(["-map", "0:v:0", "-map", "0:a?", "-map_metadata", "0"]);
    if job.container == "mkv" {
        cmd.args(["-map", "0:s?", "-c:s", "copy"]);
    }
    if let Some(filter) = job.resize.map(scale_filter) {
        cmd.args(["-vf", &filter]);
    }

    cmd.args(["-c:v", encoder]);
    let audio_kbps = job.opts.audio_kbps.max(32);
    match job.target_size.filter(|_| probe.duration > 0.0) {
        Some(target) => {
            let total_kbps = target as f64 * 8.0 / 1000.0 / probe.duration;
            // Single-pass encoders overshoot a little; leave headroom for that and the container.
            let video_kbps = (total_kbps * 0.92 - audio_kbps as f64).max(100.0) as u64;
            cmd.args(speed_args(encoder, job.effort));
            cmd.args(["-b:v", &format!("{video_kbps}k"), "-maxrate", &format!("{}k", video_kbps * 3 / 2)])
                .args(["-bufsize", &format!("{}k", video_kbps * 2)]);
        }
        None => {
            cmd.args(speed_args(encoder, job.effort));
            cmd.args(quality_args(encoder, job.opts.quality));
        }
    }
    if encoder == "libx264" {
        // 10-bit H.264 barely plays anywhere.
        cmd.args(["-pix_fmt", "yuv420p"]);
    }
    if encoder.starts_with("libx265") || encoder.starts_with("hevc_") {
        // Lets Apple devices recognize HEVC in MP4.
        cmd.args(["-tag:v", "hvc1"]);
    }
    if encoder == "libx265" {
        cmd.args(["-x265-params", "log-level=error"]);
    }

    let copy_audio = match probe.audio_codec.as_deref() {
        _ if job.target_size.is_some() => false,
        Some(_) if job.container == "mkv" => true,
        Some("aac" | "mp3" | "ac3" | "eac3" | "alac") => true,
        _ => false,
    };
    if copy_audio {
        cmd.args(["-c:a", "copy"]);
    } else {
        cmd.args(["-c:a", "aac", "-b:a", &format!("{audio_kbps}k")]);
    }
    if job.container == "mp4" {
        cmd.args(["-movflags", "+faststart", "-f", "mp4"]);
    } else {
        cmd.args(["-f", "matroska"]);
    }
    cmd.arg(job.output);

    run_with_progress(cmd, probe.duration, cancel, progress)?;
    Ok(Encoded { encoder, probe })
}

fn scale_filter(resize: Resize) -> String {
    match resize {
        Resize::Fit { max_width, max_height } => {
            let w = max_width.map_or("iw".to_string(), |w| format!("min({w}\\,iw)"));
            let h = max_height.map_or("ih".to_string(), |h| format!("min({h}\\,ih)"));
            format!("scale=w={w}:h={h}:force_original_aspect_ratio=decrease:force_divisible_by=2")
        }
        Resize::Percent(p) => {
            let f = (p.clamp(1.0, 100.0) / 100.0) as f64;
            format!("scale=w=trunc(iw*{f}/2)*2:h=trunc(ih*{f}/2)*2")
        }
    }
}

fn speed_args(encoder: &str, effort: Effort) -> Vec<String> {
    let pick = |fast: &str, balanced: &str, max: &str| match effort {
        Effort::Fast => fast.to_string(),
        Effort::Balanced => balanced.to_string(),
        Effort::Max => max.to_string(),
    };
    let args: Vec<String> = match encoder {
        "libx264" | "libx265" => vec!["-preset".into(), pick("veryfast", "medium", "slow")],
        "libsvtav1" => vec!["-preset".into(), pick("10", "8", "5")],
        e if e.ends_with("_nvenc") => vec!["-preset".into(), pick("p3", "p5", "p7"), "-tune".into(), "hq".into()],
        e if e.ends_with("_qsv") => vec!["-preset".into(), pick("veryfast", "medium", "veryslow")],
        e if e.ends_with("_amf") => vec!["-quality".into(), pick("speed", "balanced", "quality")],
        _ => vec![],
    };
    args
}

/// Maps quality 1–100 onto each encoder's constant-quality scale (80 ≈ visually transparent).
fn quality_args(encoder: &str, quality: u8) -> Vec<String> {
    let q = quality.clamp(1, 100) as f64;
    let x264_crf = (40.0 - q * 0.21).round() as i32; // 80 -> 23
    let x265_crf = x264_crf + 3; // 80 -> 26, similar visual quality
    let av1_crf = (55.0 - q * 0.29).round() as i32; // 80 -> 32
    let crf = |v: i32| v.to_string();
    match encoder {
        "libx264" => vec!["-crf".into(), crf(x264_crf)],
        "libx265" => vec!["-crf".into(), crf(x265_crf)],
        "libsvtav1" => vec!["-crf".into(), crf(av1_crf)],
        "h264_nvenc" => vec!["-rc".into(), "vbr".into(), "-cq".into(), crf(x264_crf), "-b:v".into(), "0".into()],
        "hevc_nvenc" => vec!["-rc".into(), "vbr".into(), "-cq".into(), crf(x265_crf), "-b:v".into(), "0".into()],
        "av1_nvenc" => vec!["-rc".into(), "vbr".into(), "-cq".into(), crf(av1_crf), "-b:v".into(), "0".into()],
        e if e.ends_with("_qsv") => vec!["-global_quality".into(), crf(x264_crf)],
        e if e.ends_with("_amf") => {
            let qp = if e.starts_with("av1") { av1_crf * 4 } else { x264_crf };
            vec!["-rc".into(), "cqp".into(), "-qp_i".into(), crf(qp), "-qp_p".into(), crf(qp + 2)]
        }
        e if e.ends_with("_videotoolbox") => vec!["-q:v".into(), crf((q * 0.75).round() as i32)],
        _ => vec![],
    }
}

fn run_with_progress(
    mut cmd: Command,
    duration: f64,
    cancel: &AtomicBool,
    progress: &dyn Fn(f32),
) -> Result<(), PressaError> {
    let fail = |msg: String| PressaError::new(ErrorKind::VideoFailed, msg);
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| fail(format!("can't run FFmpeg: {e}")))?;

    // Drain stderr on another thread so FFmpeg never blocks on a full pipe.
    let mut stderr = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let stdout = child.stdout.take().expect("piped");
    for line in BufReader::new(stdout).lines() {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PressaError::new(ErrorKind::Cancelled, "cancelled"));
        }
        let Ok(line) = line else { break };
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok())
            && duration > 0.0
        {
            progress((us / 1e6 / duration).clamp(0.0, 1.0) as f32);
        }
    }

    let status = child.wait().map_err(|e| fail(e.to_string()))?;
    let errors = errors.join().unwrap_or_default();
    if !status.success() {
        let msg = errors.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("FFmpeg failed");
        return Err(fail(msg.trim().to_string()));
    }
    progress(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffmpeg_fields() {
        assert_eq!(parse_timestamp("00:01:02.50"), Some(62.5));
        assert_eq!(parse_dimensions("1920x1080 [SAR 1:1 DAR 16:9]"), Some((1920, 1080)));
        assert_eq!(parse_dimensions("yuv420p(tv"), None);
    }
}
