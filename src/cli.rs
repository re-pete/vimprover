//! CLI argument parsing. Lives with the binary, not the library, so library
//! consumers don't pay for clap.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use vimprover::model::Container;
use vimprover::plan::VideoCodecChoice;

/// `vimprover` — intelligently re-encode, repair, and modernize media files.
///
/// Default invocation:
///
/// ```text
/// vimprover INPUT [INPUT...] OUTPUT
/// ```
///
/// The last positional is the output (with or without an extension; if no
/// extension, the planner picks one based on the recipe). Multi-input concat
/// will be wired up in a later build-order step. Use `--probe-only` to only
/// print a probe summary.
#[derive(Debug, Parser)]
#[command(name = "vimprover", version, about, long_about = None)]
pub struct Args {
    /// `INPUT [INPUT...] OUTPUT`. With `--probe-only`, exactly one INPUT.
    #[arg(required = true, num_args = 1..)]
    pub paths: Vec<PathBuf>,

    /// Print the probe summary for a single INPUT and exit.
    #[arg(
        short = 'p',
        long,
        conflicts_with_all = [
            "dry_run", "overwrite", "reencode", "force", "yes",
            "video_codec", "crf", "preset",
            "container", "keep_multichannel_audio",
        ],
    )]
    pub probe_only: bool,

    /// Print the plan and the exact ffmpeg command, but don't execute.
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Overwrite the output file if it already exists.
    #[arg(long)]
    pub overwrite: bool,

    /// Process the input even if assessment reports it as already fine.
    /// Without `--force`, vimprover refuses fine files in `Auto` intent mode.
    /// Explicit `--reencode` already implies forcing, so `--force` is redundant
    /// (but harmless) when combined with it.
    #[arg(short = 'f', long)]
    pub force: bool,

    /// Skip the interactive `[Y/n]` confirmation prompt and proceed directly.
    /// Required for non-interactive use (CI, batch scripts).
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Force a re-encode even when stream-copy would work. Implies `--force`
    /// (you're explicitly asking for work even if the file is fine).
    #[arg(short = 'r', long)]
    pub reencode: bool,

    /// Video codec for re-encode. Default: x264 ≤1080p SDR, x265 above or HDR.
    #[arg(long, value_enum)]
    pub video_codec: Option<CliCodec>,

    /// Constant Rate Factor (lower = higher quality, larger files).
    /// Sane range: 18–28 for x264, 20–30 for x265.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=51))]
    pub crf: Option<u8>,

    /// Encoder preset (`ultrafast`…`veryslow`). Slower presets compress better
    /// at the cost of encode time. Default: `medium`.
    #[arg(long)]
    pub preset: Option<String>,

    /// Output container. Default: `mkv` (most permissive).
    #[arg(long, value_enum)]
    pub container: Option<CliContainer>,

    /// Re-encode multichannel audio to AAC multichannel instead of downmixing
    /// to AAC stereo (the default).
    #[arg(long)]
    pub keep_multichannel_audio: bool,
}

/// Output container choice exposed on the CLI.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliContainer {
    Mkv,
    Mp4,
}

impl From<CliContainer> for Container {
    fn from(c: CliContainer) -> Self {
        match c {
            CliContainer::Mkv => Container::Mkv,
            CliContainer::Mp4 => Container::Mp4,
        }
    }
}

/// Video codec choice exposed on the CLI.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliCodec {
    X264,
    X265,
}

impl From<CliCodec> for VideoCodecChoice {
    fn from(c: CliCodec) -> Self {
        match c {
            CliCodec::X264 => VideoCodecChoice::X264,
            CliCodec::X265 => VideoCodecChoice::X265,
        }
    }
}
