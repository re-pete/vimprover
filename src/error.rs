//! Typed library errors. The binary wraps these in `anyhow::Error` at its boundary.

use std::path::PathBuf;

use thiserror::Error;

/// Convenience `Result` alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// All errors produced by the `vimprover` library.
#[derive(Debug, Error)]
pub enum Error {
    #[error("ffprobe binary not found; install ffmpeg or set VIMPROVER_FFPROBE")]
    FfprobeNotFound(#[source] which::Error),

    #[error("ffmpeg binary not found; install ffmpeg or set VIMPROVER_FFMPEG")]
    FfmpegNotFound(#[source] which::Error),

    #[error("input file does not exist: {0}")]
    InputNotFound(PathBuf),

    #[error("input path is not a regular file: {0}")]
    InputNotFile(PathBuf),

    #[error("ffprobe failed on {path} (exit {status}):\n{stderr}")]
    FfprobeFailed {
        path: PathBuf,
        status: i32,
        stderr: String,
    },

    #[error("could not parse ffprobe output for {path}: {source}")]
    FfprobeParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("ffprobe reported no streams for {0}")]
    NoStreams(PathBuf),

    #[error("ffprobe reported no video stream for {0}")]
    NoVideoStream(PathBuf),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}
