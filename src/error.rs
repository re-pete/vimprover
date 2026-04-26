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

    #[error("output file already exists: {0} (pass --overwrite to replace)")]
    OutputExists(PathBuf),

    #[error("ffmpeg failed (exit {status}); see the output above for details")]
    FfmpegFailed { status: i32 },

    #[error("{0} is not yet implemented")]
    Unimplemented(&'static str),

    /// Returned by `Intent::Shrink` planning when the source is already
    /// at-or-below the target threshold and the user gave no explicit knobs:
    /// there's literally no smaller file we'd produce. The CLI surfaces this
    /// as a refusal with a hint about `--target-bitrate` / `--max-height`.
    #[error(
        "{0} \u{2014} pass --target-bitrate or --max-height to force a specific shrink target"
    )]
    NothingToShrink(String),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}
