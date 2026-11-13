use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use pressa_core::{
    ConvertOptions, Effort, OutputFormat, Resize, VideoCodec, VideoOptions, collect_inputs, convert_batch,
    encoder_label, ffmpeg_path, pick_encoder,
};

/// Convert and compress images, and compress videos.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Files or folders to convert. Folders are mirrored with their subfolders.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Image output format (jpg, png, webp, avif, gif, bmp, tiff, ico, tga, qoi). Default: keep.
    #[arg(short, long, value_parser = parse_format)]
    format: Option<OutputFormat>,

    /// Quality 1–100 for lossy formats.
    #[arg(short, long, default_value_t = 80, value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: u8,

    /// Lossless output (PNG, WebP, and formats that are always lossless).
    #[arg(short, long)]
    lossless: bool,

    /// Speed/size trade-off.
    #[arg(short, long, value_enum, default_value_t = EffortArg::Balanced)]
    effort: EffortArg,

    /// Maximum width in pixels (keeps aspect ratio, never upscales).
    #[arg(long)]
    max_width: Option<u32>,

    /// Maximum height in pixels (keeps aspect ratio, never upscales).
    #[arg(long)]
    max_height: Option<u32>,

    /// Scale to this percentage of the original size.
    #[arg(long, conflicts_with_all = ["max_width", "max_height"])]
    scale: Option<f32>,

    /// Target file size, e.g. 500KB or 1.5MB (picks the best quality that fits).
    #[arg(short, long, value_parser = parse_size)]
    target_size: Option<u64>,

    /// Video codec: h264, h265 or av1.
    #[arg(long, default_value = "h265", value_parser = parse_codec)]
    video_codec: VideoCodec,

    /// Video quality 1–100.
    #[arg(long, default_value_t = 70, value_parser = clap::value_parser!(u8).range(1..=100))]
    video_quality: u8,

    /// Encode videos on the CPU even if a GPU encoder is available.
    #[arg(long)]
    no_gpu: bool,

    /// Output folder. Default: next to each input file; added folders go to a `<name>-pressa` copy.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Overwrite existing files, including the inputs themselves.
    #[arg(long)]
    overwrite: bool,

    /// Search folders recursively.
    #[arg(short, long)]
    recursive: bool,

    /// Number of parallel jobs. Default: number of CPU cores.
    #[arg(short, long)]
    jobs: Option<usize>,
}

#[derive(Clone, Copy, ValueEnum)]
enum EffortArg {
    Fast,
    Balanced,
    Max,
}

fn parse_format(s: &str) -> Result<OutputFormat> {
    OutputFormat::from_name(s).ok_or_else(|| anyhow!("unknown output format '{s}'"))
}

fn parse_codec(s: &str) -> Result<VideoCodec> {
    VideoCodec::from_name(s).ok_or_else(|| anyhow!("unknown video codec '{s}' (use h264, h265 or av1)"))
}

fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim().to_ascii_uppercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: f64 = num.parse().map_err(|_| anyhow!("invalid size '{s}'"))?;
    let mult = match unit.trim() {
        "" | "B" => 1.0,
        "K" | "KB" => 1024.0,
        "M" | "MB" => 1024.0 * 1024.0,
        _ => bail!("unknown size unit '{unit}' (use B, KB or MB)"),
    };
    Ok((num * mult) as u64)
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.2} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool> {
    let args = Args::parse();
    if let Some(jobs) = args.jobs {
        rayon::ThreadPoolBuilder::new().num_threads(jobs).build_global()?;
    }
    let resize = match (args.scale, args.max_width, args.max_height) {
        (Some(p), _, _) if (1.0..=100.0).contains(&p) => Some(Resize::Percent(p)),
        (Some(_), _, _) => bail!("--scale must be between 1 and 100"),
        (None, None, None) => None,
        (None, max_width, max_height) => Some(Resize::Fit { max_width, max_height }),
    };
    let opts = ConvertOptions {
        format: args.format,
        quality: args.quality,
        lossless: args.lossless,
        effort: match args.effort {
            EffortArg::Fast => Effort::Fast,
            EffortArg::Balanced => Effort::Balanced,
            EffortArg::Max => Effort::Max,
        },
        resize,
        target_size: args.target_size,
        output_dir: args.output,
        overwrite: args.overwrite,
        video: VideoOptions {
            codec: args.video_codec,
            quality: args.video_quality,
            hardware: !args.no_gpu,
            ..Default::default()
        },
    };

    let files = collect_inputs(&args.inputs, args.recursive);
    if files.is_empty() {
        bail!("no supported images or videos found");
    }
    if files.iter().any(|f| f.kind() == pressa_core::MediaKind::Video) {
        match pick_encoder(&opts.video) {
            Some(enc) => eprintln!(
                "Videos: {} via {} ({enc}), FFmpeg at {}",
                opts.video.codec.name(),
                encoder_label(enc),
                ffmpeg_path().map(|p| p.display().to_string()).unwrap_or_default()
            ),
            None => eprintln!("Videos will fail: FFmpeg not found (bundle it next to pressa-cli or add it to PATH)"),
        }
    }

    let start = Instant::now();
    let totals = Mutex::new((0u64, 0u64, 0usize, 0usize)); // in, out, ok, failed
    let on_progress = |_: usize, _: f32| {};
    convert_batch(&files, &opts, &AtomicBool::new(false), on_progress, |i, result| {
        let mut t = totals.lock().unwrap();
        match result {
            Ok(r) => {
                t.0 += r.input_size;
                t.1 += r.output_size;
                t.2 += 1;
                let mut notes = Vec::new();
                if let Some(q) = r.quality_used {
                    notes.push(format!("q={q}"));
                }
                if r.kept_original {
                    notes.push("kept original".into());
                }
                if r.target_missed {
                    notes.push("target size not reached".into());
                }
                println!(
                    "{} -> {}  [{}]  {} -> {} ({:+.1}%)  {}x{}  {:.2}s{}",
                    r.input.display(),
                    r.output.display(),
                    r.output_format,
                    human(r.input_size),
                    human(r.output_size),
                    r.change_percent(),
                    r.width,
                    r.height,
                    r.elapsed.as_secs_f64(),
                    if notes.is_empty() { String::new() } else { format!("  [{}]", notes.join(", ")) },
                );
            }
            Err(e) => {
                t.3 += 1;
                eprintln!("FAILED {}: {e:#}", files[i].path.display());
            }
        }
    });

    let (input, output, ok, failed) = *totals.lock().unwrap();
    let change = if input > 0 { (output as f64 / input as f64 - 1.0) * 100.0 } else { 0.0 };
    println!(
        "\n{ok} converted, {failed} failed in {:.2}s. Total {} -> {} ({change:+.1}%)",
        start.elapsed().as_secs_f64(),
        human(input),
        human(output),
    );
    Ok(failed == 0)
}
