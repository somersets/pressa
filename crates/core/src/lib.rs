//! Image decoding, transformation and size-optimized encoding.

mod decode;
mod encode;
mod format;
mod pipeline;
mod transform;

pub use decode::{Decoded, decode};
pub use encode::{Effort, EncodeSettings, encode};
pub use format::{InputFormat, OutputFormat, RAW_EXTENSIONS, detect, is_supported_path};
pub use pipeline::{ConvertOptions, Report, collect_inputs, convert_batch, convert_file};
pub use transform::Resize;
