use std::fmt;

/// What went wrong, so front ends can show a translated message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    UnsupportedFormat,
    ReadFailed,
    DecodeFailed,
    EncodeFailed,
    WriteFailed,
    FfmpegMissing,
    VideoFailed,
    Cancelled,
}

/// Error with a kind for the UI and technical details for logs and tooltips.
#[derive(Debug)]
pub struct PressaError {
    pub kind: ErrorKind,
    pub detail: String,
}

impl PressaError {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self { kind, detail: detail.into() }
    }
}

impl fmt::Display for PressaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for PressaError {}

/// The kind of an error returned by the pipeline, if it has one.
pub fn error_kind(err: &anyhow::Error) -> Option<ErrorKind> {
    err.downcast_ref::<PressaError>().map(|e| e.kind)
}

pub(crate) trait ResultExt<T> {
    /// Tags any error with a kind, keeping its full message as the detail.
    fn kind(self, kind: ErrorKind) -> anyhow::Result<T>;
}

impl<T, E: Into<anyhow::Error>> ResultExt<T> for Result<T, E> {
    fn kind(self, kind: ErrorKind) -> anyhow::Result<T> {
        self.map_err(|e| {
            let e: anyhow::Error = e.into();
            match e.downcast::<PressaError>() {
                Ok(tagged) => tagged.into(),
                Err(e) => PressaError::new(kind, format!("{e:#}")).into(),
            }
        })
    }
}
