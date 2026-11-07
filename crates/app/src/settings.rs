use std::path::PathBuf;

use pressa_core::{ConvertOptions, Effort, OutputFormat, Resize};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResizeMode {
    None,
    Fit,
    Percent,
}

/// User settings, persisted between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Output format extension; `None` keeps the input format.
    pub format: Option<String>,
    pub quality: u8,
    pub lossless: bool,
    pub effort: u8,
    pub resize_mode: ResizeMode,
    pub max_width: u32,
    pub max_height: u32,
    pub percent: f32,
    pub target_enabled: bool,
    pub target_kb: u32,
    pub output_dir: Option<PathBuf>,
    pub overwrite: bool,
    pub recursive: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: Some("webp".into()),
            quality: 80,
            lossless: false,
            effort: 1,
            resize_mode: ResizeMode::None,
            max_width: 1920,
            max_height: 1920,
            percent: 50.0,
            target_enabled: false,
            target_kb: 500,
            output_dir: None,
            overwrite: false,
            recursive: true,
        }
    }
}

impl Settings {
    pub fn output_format(&self) -> Option<OutputFormat> {
        self.format.as_deref().and_then(OutputFormat::from_name)
    }

    pub fn set_output_format(&mut self, format: Option<OutputFormat>) {
        self.format = format.map(|f| f.extension().to_string());
    }

    pub fn effort(&self) -> Effort {
        Effort::ALL[(self.effort as usize).min(Effort::ALL.len() - 1)]
    }

    pub fn to_options(&self) -> ConvertOptions {
        ConvertOptions {
            format: self.output_format(),
            quality: self.quality.clamp(1, 100),
            lossless: self.lossless,
            effort: self.effort(),
            resize: match self.resize_mode {
                ResizeMode::None => None,
                ResizeMode::Fit => Some(Resize::Fit {
                    max_width: Some(self.max_width.max(1)),
                    max_height: Some(self.max_height.max(1)),
                }),
                ResizeMode::Percent => Some(Resize::Percent(self.percent.clamp(1.0, 100.0))),
            },
            target_size: self.target_enabled.then_some(self.target_kb.max(1) as u64 * 1024),
            output_dir: self.output_dir.clone(),
            overwrite: self.overwrite,
        }
    }
}
