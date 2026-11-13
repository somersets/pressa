//! Image and video decoding, transformation and size-optimized encoding.

mod decode;
mod encode;
mod error;
mod format;
mod pipeline;
mod transform;
mod video;

pub use decode::{Decoded, decode};
pub use encode::{Effort, EncodeSettings, encode};
pub use error::{ErrorKind, PressaError, error_kind};
pub use format::{InputFormat, OutputFormat, RAW_EXTENSIONS, detect, is_supported_path};
pub use pipeline::{
    ConvertOptions, InputFile, MIRROR_SUFFIX, MediaKind, Report, collect_inputs, convert_batch,
    convert_file, is_supported, output_dir,
};
pub use transform::Resize;
pub use video::{VIDEO_EXTENSIONS, VideoCodec, VideoOptions, encoder_label, ffmpeg_path, pick_encoder};
