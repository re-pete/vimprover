//! Step 5 of the pipeline (stub): run ffmpeg according to an [`EncodeRecipe`].
//!
//! Implementation notes from `CLAUDE.md`:
//!
//! - Build args as `Vec<String>`; never shell-string concat (path injection risk).
//! - Don't use `Command::output()` for long-running processes — spawn with
//!   piped stdio and read incrementally.
//! - Parse `-progress pipe:1` output line-by-line (`key=value`, `progress=end`).
//! - Capture stderr; surface on failure.
//! - Support multi-step pipelines for concat mode.
//! - Log exact ffmpeg commands at debug level for "paste into terminal" debugging.

use std::env;
use std::path::PathBuf;

use crate::error::{Error, Result};

/// Locate the ffmpeg binary, preferring the `VIMPROVER_FFMPEG` env override.
pub fn locate_ffmpeg() -> Result<PathBuf> {
    if let Ok(override_path) = env::var("VIMPROVER_FFMPEG") {
        if !override_path.is_empty() {
            return Ok(PathBuf::from(override_path));
        }
    }
    which::which("ffmpeg").map_err(Error::FfmpegNotFound)
}
