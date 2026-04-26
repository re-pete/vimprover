//! CLI argument parsing. Lives with the binary, not the library, so library
//! consumers don't pay for clap.

use std::path::PathBuf;

use clap::Parser;

/// `vimprover` — intelligently re-encode, repair, and modernize media files.
///
/// Step 1 of development: probe each INPUT and print a summary.
#[derive(Debug, Parser)]
#[command(name = "vimprover", version, about, long_about = None)]
pub struct Args {
    /// Input media file(s). Multiple inputs will later trigger concat mode.
    #[arg(required = true, num_args = 1..)]
    pub inputs: Vec<PathBuf>,
}
