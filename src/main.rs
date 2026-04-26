//! `vimprover` binary entry point.
//!
//! Build-order step 2 wires together the full pipeline that exists today:
//!
//! 1. Parse CLI args.
//! 2. Decide between `--probe-only` mode and the normal `INPUT... OUTPUT` flow.
//! 3. For each input: run [`vimprover::probe::probe_file`] and print the profile.
//! 4. Plan a recipe via [`vimprover::plan::plan`] (Auto intent for now).
//! 5. Resolve the output path, render the plan, optionally print the exact
//!    ffmpeg command line.
//! 6. Unless `--dry-run`, run [`vimprover::execute::run_recipe`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use vimprover::assess::Assessment;
use vimprover::execute;
use vimprover::format;
use vimprover::model::MediaProfile;
use vimprover::plan::{self, Intent, Overrides};
use vimprover::probe;

use crate::cli::Args;

mod cli;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    init_tracing();

    let args = cli::Args::parse();

    if args.probe_only {
        return run_probe_only(&args.paths).await;
    }

    if args.paths.len() < 2 {
        bail!(
            "expected `INPUT [INPUT...] OUTPUT` (got {} path{})",
            args.paths.len(),
            if args.paths.len() == 1 { "" } else { "s" }
        );
    }

    let mut paths = args.paths.clone();
    let user_output = paths.pop().expect("checked len >= 2");
    let inputs = paths;

    if inputs.len() > 1 {
        bail!(
            "concat mode (multiple inputs) is not implemented yet \
             — coming in build-order step 6"
        );
    }

    let intent = intent_from_args(&args);
    let overrides = overrides_from_args(&args, &user_output);

    run_single_file(
        &inputs[0],
        &user_output,
        intent,
        overrides,
        args.dry_run,
        args.overwrite,
    )
    .await
}

fn intent_from_args(args: &Args) -> Intent {
    if args.reencode {
        Intent::Reencode
    } else {
        Intent::Auto
    }
}

fn overrides_from_args(args: &Args, output_path: &Path) -> Overrides {
    // Explicit --container wins; otherwise infer from a recognized extension
    // on the output path (e.g. `out.mp4` → MP4). Per CLAUDE.md: "OUTPUT_NAME
    // without extension lets the planner pick the container. With extension
    // forces it."
    let container = args
        .container
        .map(Into::into)
        .or_else(|| plan::container_from_output_extension(output_path));

    Overrides {
        container,
        video_codec: args.video_codec.map(Into::into),
        crf: args.crf,
        preset: args.preset.clone(),
        max_height: None, // wired up in build-order step 5 (shrink)
        keep_multichannel_audio: args.keep_multichannel_audio,
    }
}

async fn run_probe_only(paths: &[PathBuf]) -> Result<()> {
    if paths.len() != 1 {
        bail!(
            "--probe-only takes exactly one INPUT (got {})",
            paths.len()
        );
    }
    let path = &paths[0];
    let profile = probe::probe_file(path)
        .await
        .with_context(|| format!("probing {}", path.display()))?;
    println!("{}", format::render_profile(path, &profile));
    Ok(())
}

async fn run_single_file(
    input: &Path,
    user_output: &Path,
    intent: Intent,
    overrides: Overrides,
    dry_run: bool,
    overwrite: bool,
) -> Result<()> {
    // 1. Probe.
    let profile: MediaProfile = probe::probe_file(input)
        .await
        .with_context(|| format!("probing {}", input.display()))?;
    println!("{}", format::render_profile(input, &profile));

    // 2. Plan.
    let assessment = Assessment::default(); // wired up in step 4
    let recipe = plan::plan(&profile, &assessment, &intent, &overrides)
        .with_context(|| "planning")?;

    // 3. Resolve output path.
    let output = plan::resolve_output_path(user_output, &recipe);

    // 4. Render the plan.
    println!();
    println!("{}", format::render_recipe(&recipe, &output));

    // 5. Dry-run short-circuit: render the exact ffmpeg command and exit.
    if dry_run {
        let ffmpeg = execute::locate_ffmpeg()?;
        let argv = execute::build_ffmpeg_args(
            &[input],
            std::slice::from_ref(&profile.container),
            &output,
            &recipe,
            overwrite,
        );
        println!();
        println!("Command:   {}", execute::render_command(&ffmpeg, &argv));
        println!();
        println!("(dry run — not executing)");
        return Ok(());
    }

    // 6. Execute.
    println!();
    println!("Running ffmpeg…");
    execute::run_recipe(
        &[input],
        std::slice::from_ref(&profile.container),
        &output,
        &recipe,
        overwrite,
    )
    .await
    .with_context(|| format!("encoding to {}", output.display()))?;

    println!("Done. Wrote {}.", output.display());
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
