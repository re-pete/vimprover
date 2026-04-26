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

    /// A `.partial.<ext>` sibling of the target output already exists,
    /// left over from a prior ffmpeg run that failed or was interrupted.
    /// Refused (rather than silently clobbered) so the user can either
    /// inspect/recover it or opt in to replacing via `--overwrite`.
    #[error(
        "partial output from a prior run exists: {0} \
         (pass --overwrite to replace, or remove it to retry)"
    )]
    PartialOutputExists(PathBuf),

    /// The atomic rename from `<output>.partial.<ext>` to the final output
    /// path failed. Almost always a cross-filesystem rename (e.g. tmpfs
    /// output directory) or a permissions issue. The encoded payload is
    /// preserved at `from` for the user to salvage manually.
    #[error(
        "encode succeeded but renaming {from} to {to} failed: {source} \
         (the encoded file is preserved at {from}; mv it into place manually)"
    )]
    RenameFailed {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },

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

    /// Returned by [`crate::plan::plan_concat`] when the inputs have
    /// non-uniform stream parameters. Demuxer-mode concat requires every
    /// input to match exactly; we refuse rather than silently produce a
    /// broken output. The error carries the offending input's path and a
    /// human-readable description of the first field that differed.
    #[error(
        "concat refused: {path} differs from input #1 in {why}. \
         Demuxer-mode concat requires uniform streams; normalize the \
         differing input first (e.g. with a single-file `vimprover --reencode` \
         or `vimprover --intent shrink --max-height ...` run), then retry."
    )]
    ConcatInputsDiffer { path: PathBuf, why: String },

    /// Returned when a concat operation is requested with fewer than two
    /// inputs. The CLI shouldn't ever produce this — it's a last-line-of-
    /// defense for library callers.
    #[error("concat needs at least two inputs (got {0})")]
    ConcatTooFewInputs(usize),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}
