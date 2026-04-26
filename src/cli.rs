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
            "intent", "max_height", "target_bitrate",
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
    /// Mutually exclusive with `--intent`.
    #[arg(short = 'r', long, conflicts_with = "intent")]
    pub reencode: bool,

    /// Explicit pipeline intent. When omitted, intent is `auto` (assessment
    /// drives the choice between remux and re-encode). `--reencode` is a
    /// shorthand for `--intent reencode`.
    #[arg(long, value_enum)]
    pub intent: Option<CliIntent>,

    /// Cap output height in pixels (downscales if the source is taller).
    /// Only meaningful with `--intent shrink` (or implicit shrink). Common
    /// values: 720, 1080, 1440.
    #[arg(long, value_parser = clap::value_parser!(u32).range(120..=8192))]
    pub max_height: Option<u32>,

    /// Target output bitrate in bits/sec for shrink-mode ABR encoding.
    /// Accepts a plain integer (`5000000`) or a suffixed value (`5M`, `750k`).
    /// Only meaningful with `--intent shrink`.
    #[arg(long, value_parser = parse_bitrate)]
    pub target_bitrate: Option<u64>,

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

/// Pipeline intent exposed on the CLI. Maps 1:1 onto the library's
/// `Intent` enum (with shrink params filled in from `--max-height` /
/// `--target-bitrate` at the binary boundary). `concat` is left out because
/// concat is implicit when ≥2 inputs are passed (build-order step 6).
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliIntent {
    Auto,
    Remux,
    Reencode,
    Shrink,
}

/// Parse a bitrate string like `5M`, `750k`, or `5000000` into bits/sec.
///
/// Accepts the suffixes `k`/`K` (× 1 000), `m`/`M` (× 1 000 000) and
/// `g`/`G` (× 1 000 000 000). Decimal points are allowed before the suffix
/// (`1.5M` → 1 500 000). Plain integers are interpreted as bits/sec.
fn parse_bitrate(s: &str) -> Result<u64, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("empty bitrate".into());
    }

    let (num_str, multiplier) = match trimmed.chars().last() {
        Some('k' | 'K') => (&trimmed[..trimmed.len() - 1], 1_000_u64),
        Some('m' | 'M') => (&trimmed[..trimmed.len() - 1], 1_000_000_u64),
        Some('g' | 'G') => (&trimmed[..trimmed.len() - 1], 1_000_000_000_u64),
        _ => (trimmed, 1_u64),
    };

    let num: f64 = num_str
        .parse()
        .map_err(|_| format!("could not parse bitrate '{s}' as a number"))?;
    if num < 0.0 || !num.is_finite() {
        return Err(format!("bitrate must be a positive finite number, got '{s}'"));
    }

    let bps = (num * multiplier as f64).round();
    if bps > u64::MAX as f64 {
        return Err(format!("bitrate '{s}' is unreasonably large"));
    }
    Ok(bps as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_bitrate_accepts_plain_integer() {
        assert_eq!(parse_bitrate("5000000").unwrap(), 5_000_000);
        assert_eq!(parse_bitrate("0").unwrap(), 0);
    }

    #[test]
    fn parse_bitrate_accepts_si_suffixes() {
        assert_eq!(parse_bitrate("5M").unwrap(), 5_000_000);
        assert_eq!(parse_bitrate("5m").unwrap(), 5_000_000);
        assert_eq!(parse_bitrate("750k").unwrap(), 750_000);
        assert_eq!(parse_bitrate("2G").unwrap(), 2_000_000_000);
    }

    #[test]
    fn parse_bitrate_accepts_decimals() {
        assert_eq!(parse_bitrate("1.5M").unwrap(), 1_500_000);
        assert_eq!(parse_bitrate("2.5k").unwrap(), 2_500);
    }

    #[test]
    fn parse_bitrate_rejects_garbage() {
        assert!(parse_bitrate("").is_err());
        assert!(parse_bitrate("abc").is_err());
        assert!(parse_bitrate("-5M").is_err());
        assert!(parse_bitrate("5MM").is_err());
    }
}
