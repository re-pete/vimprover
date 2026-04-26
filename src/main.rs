//! `vimprover` binary entry point.
//!
//! Today this is a thin wrapper around [`vimprover::probe::probe_file`]: it
//! parses CLI args, sets up logging, probes each input, and prints the result.

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use vimprover::{format, probe};

mod cli;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    init_tracing();

    let args = cli::Args::parse();

    let mut first = true;
    for input in &args.inputs {
        if !first {
            println!();
        }
        first = false;

        let profile = probe::probe_file(input)
            .await
            .with_context(|| format!("probing {}", input.display()))?;
        println!("{}", format::render_profile(input, &profile));
    }

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("VIMPROVER_LOG")
        .unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .init();
}
