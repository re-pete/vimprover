//! CLI argument parsing. Lives with the binary, not the library, so library
//! consumers don't pay for clap.

use std::path::PathBuf;

use clap::Parser;

/// `vimprover` — intelligently re-encode, repair, and modernize media files.
///
/// Default invocation:
///
/// ```text
/// vimprover INPUT [INPUT...] OUTPUT
/// ```
///
/// The last positional is the output (with or without an extension; if no
/// extension, the planner picks one). Multi-input concat will be wired up in a
/// later build-order step. Use `--probe-only` to only print a probe summary.
#[derive(Debug, Parser)]
#[command(name = "vimprover", version, about, long_about = None)]
pub struct Args {
    /// `INPUT [INPUT...] OUTPUT`. With `--probe-only`, exactly one INPUT.
    #[arg(required = true, num_args = 1..)]
    pub paths: Vec<PathBuf>,

    /// Print the probe summary for a single INPUT and exit.
    #[arg(short = 'p', long, conflicts_with_all = ["dry_run", "overwrite"])]
    pub probe_only: bool,

    /// Print the plan and the exact ffmpeg command, but don't execute.
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Overwrite the output file if it already exists.
    #[arg(short = 'f', long)]
    pub overwrite: bool,
}
