//! CLI argument parsing. Lives with the binary, not the library, so library
//! consumers don't pay for clap.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use vimprover::model::Container;
use vimprover::plan::VideoCodecChoice;

/// Top-level CLI args. The doc-comment becomes `--help` text.
#[derive(Debug, Parser)]
#[command(
    name = "vimprover",
    version,
    about = "Intelligently re-encode, repair, and modernize media files.",
    long_about = LONG_ABOUT,
    after_long_help = LONG_EXAMPLES,
)]
pub struct Args {
    /// Inputs followed by output. The last positional is always the output.
    ///
    /// One INPUT → single-file mode (probe + assess + plan + encode).
    /// Two or more INPUTs → concat mode (stream-copy join, demuxer demands
    /// uniform inputs). With `--probe-only` or `--upgrade`, exactly one
    /// INPUT and no OUTPUT.
    ///
    /// The output may be given with or without an extension. When omitted,
    /// vimprover picks the canonical extension for the chosen container
    /// (`.mkv` by default).
    #[arg(required = true, num_args = 1..)]
    pub paths: Vec<PathBuf>,

    // -----------------------------------------------------------------------
    // Run control
    // -----------------------------------------------------------------------
    /// Print the probe summary for a single INPUT and exit.
    #[arg(
        short = 'p',
        long,
        help_heading = "Run control",
        conflicts_with_all = [
            "dry_run", "overwrite", "reencode", "force", "yes", "upgrade",
            "intent", "max_height", "target_bitrate",
            "video_codec", "crf", "preset",
            "container", "keep_multichannel_audio",
        ],
    )]
    pub probe_only: bool,

    /// Upgrade a single INPUT in place. No OUTPUT is given; vimprover
    /// writes the upgraded file next to the input with the chosen
    /// container's extension (e.g. `myfile.wmv` → `myfile.mkv`). When the
    /// output would collide with the input (same container), the original
    /// is renamed aside to `<stem>.vimprover-orig.<ext>` before encoding.
    /// On any failure, the backup is restored to its original name.
    ///
    /// Refuses if the output path or backup path already exists unless
    /// `--overwrite` is passed. Incompatible with concat (multi-input) mode.
    #[arg(long, help_heading = "Run control")]
    pub upgrade: bool,

    /// Print the plan and the exact ffmpeg command, but don't execute.
    #[arg(short = 'n', long, help_heading = "Run control")]
    pub dry_run: bool,

    /// Skip the interactive `[Y/n]` confirmation and proceed directly.
    /// Required for non-interactive use (CI, batch scripts).
    #[arg(short = 'y', long, help_heading = "Run control")]
    pub yes: bool,

    /// Overwrite the output if it already exists. Also clears any leftover
    /// `<output>.partial.<ext>` from a prior failed run.
    #[arg(long, help_heading = "Run control")]
    pub overwrite: bool,

    /// Process the input even if assessment reports it as already fine.
    /// Only relevant in `--intent auto` (the default); explicit intents
    /// like `--reencode` / `--intent shrink` already bypass the fine-gate.
    #[arg(short = 'f', long, help_heading = "Run control")]
    pub force: bool,

    // -----------------------------------------------------------------------
    // Intent
    // -----------------------------------------------------------------------
    /// Explicit pipeline intent. Without this, intent is `auto` for one
    /// input and `concat` for many.
    ///
    /// - `auto`     — assessment drives remux vs re-encode (default for 1 input)
    /// - `remux`    — change container, never re-encode
    /// - `reencode` — full re-encode regardless of assessment
    /// - `shrink`   — re-encode at lower bitrate / resolution (see `--max-height` / `--target-bitrate`)
    /// - `concat`   — multi-input join (default for ≥2 inputs)
    #[arg(
        long,
        value_enum,
        help_heading = "Intent",
        verbatim_doc_comment,
    )]
    pub intent: Option<CliIntent>,

    /// Shorthand for `--intent reencode`. Mutually exclusive with `--intent`.
    #[arg(short = 'r', long, conflicts_with = "intent", help_heading = "Intent")]
    pub reencode: bool,

    /// Cap output height in pixels (downscale if the source is taller).
    /// Implies `--intent shrink` when used alone. Common values: 720, 1080, 1440.
    #[arg(
        long,
        value_parser = clap::value_parser!(u32).range(120..=8192),
        help_heading = "Intent",
    )]
    pub max_height: Option<u32>,

    /// Target output bitrate for shrink-mode ABR encoding. Accepts a plain
    /// integer (`5000000`) or a suffixed value (`5M`, `750k`, `1.5M`).
    /// Implies `--intent shrink` when used alone.
    #[arg(long, value_parser = parse_bitrate, help_heading = "Intent")]
    pub target_bitrate: Option<u64>,

    // -----------------------------------------------------------------------
    // Encoding
    // -----------------------------------------------------------------------
    /// Video codec for re-encode. Default: x264 for ≤1080p SDR, x265 above
    /// or for HDR sources.
    #[arg(long, value_enum, help_heading = "Encoding")]
    pub video_codec: Option<CliCodec>,

    /// Constant Rate Factor (lower = higher quality, larger files).
    /// Reasonable range: 18–28 for x264, 20–30 for x265.
    #[arg(
        long,
        value_parser = clap::value_parser!(u8).range(0..=51),
        help_heading = "Encoding",
    )]
    pub crf: Option<u8>,

    /// Encoder preset (`ultrafast`…`veryslow`). Slower presets compress
    /// better at the cost of encode time. Default: `medium`.
    #[arg(long, help_heading = "Encoding")]
    pub preset: Option<String>,

    /// Output container. Default: `mkv` (most permissive). Can also be
    /// inferred from the output extension.
    #[arg(long, value_enum, help_heading = "Encoding")]
    pub container: Option<CliContainer>,

    /// Re-encode multichannel audio to AAC multichannel instead of
    /// downmixing to AAC stereo (the default).
    #[arg(long, help_heading = "Encoding")]
    pub keep_multichannel_audio: bool,
}

/// Long top-level description, shown on `--help`.
const LONG_ABOUT: &str = "\
Intelligently re-encode, repair, and modernize media files.

vimprover probes input files, decides what (if anything) needs to be done
to bring them up to modern standards, and either does it or explains why
it won't.

Modes (auto-detected from input count, overridable with --intent):
  - 1 input   → assess + remux/re-encode/shrink the file
  - --upgrade → 1 input, output name is auto-computed next to the input
                (original is preserved, renamed aside if the container
                doesn't change)
  - ≥2 inputs → concat them with ffmpeg's demuxer (stream-copy, requires
                uniform stream parameters across inputs)

Output is written atomically: ffmpeg writes to `<output>.partial.<ext>`
and we rename it into place on success, so a Ctrl-C halfway through
won't leave a corrupt file at the user-visible path.";

/// Examples shown after `--help`.
const LONG_EXAMPLES: &str = "\
Examples:
  # Upgrade a legacy file in place (original preserved as myfile.wmv):
  vimprover --upgrade myfile.wmv

  # Upgrade an MKV that needs a re-encode; original is renamed aside to
  # myfile.vimprover-orig.mkv before encoding, and the new MKV takes its
  # place on success:
  vimprover --upgrade --intent shrink myfile.mkv

  # Auto-modernize with an explicit output name:
  vimprover old-movie.vob newname

  # Just print the probe summary, do nothing:
  vimprover --probe-only some-movie.mkv

  # Print the plan and the exact ffmpeg command, then exit:
  vimprover --dry-run old-movie.vob newname

  # Force a re-encode regardless of assessment:
  vimprover --reencode --yes old-movie.vob newname

  # Shrink: cap bitrate at the per-resolution threshold (DWIM):
  vimprover --intent shrink --yes huge-1080p.mkv smaller

  # Shrink: downscale to 720p (implies --intent shrink):
  vimprover --max-height 720 --yes huge-1080p.mkv smaller

  # Shrink: explicit target bitrate:
  vimprover --target-bitrate 2.5M --yes huge.mkv smaller

  # Force x265 + MP4, custom CRF, slower preset for better compression:
  vimprover --reencode --video-codec x265 --container mp4 \\
            --crf 22 --preset slow --yes old-movie.vob newname

  # Concat (multi-input → single output, demuxer-mode stream-copy):
  vimprover --yes part1.mp4 part2.mp4 part3.mp4 joined

  # Verbose tracing (logs the exact ffprobe/ffmpeg commands):
  VIMPROVER_LOG=info vimprover --yes old-movie.vob newname

Environment:
  VIMPROVER_FFMPEG    path to ffmpeg binary (default: from $PATH)
  VIMPROVER_FFPROBE   path to ffprobe binary (default: from $PATH)
  VIMPROVER_LOG       tracing-subscriber filter (default: warn)";

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
/// `--target-bitrate` at the binary boundary).
///
/// `Concat` is also implicit when ≥2 inputs are passed; specifying
/// `--intent concat` is just an explicit form for scripts.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliIntent {
    Auto,
    Remux,
    Reencode,
    Shrink,
    Concat,
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
