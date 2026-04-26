//! `vimprover` binary entry point.
//!
//! Step 4 wiring:
//!
//! 1. Parse CLI args.
//! 2. `--probe-only` short-circuit prints just the probe and exits.
//! 3. Probe the input → print the profile.
//! 4. [`vimprover::assess::assess`] → print the `Issues:` block (if any).
//! 5. **Fine-gate**: in `Intent::Auto` mode, refuse fine files unless
//!    `--force` was passed.
//! 6. [`vimprover::plan::plan`] → render the plan.
//! 7. `--dry-run` short-circuits with the exact ffmpeg command and exits.
//! 8. **Confirmation prompt** (unless `--yes`): ask `[Y/n]` and require
//!    `--yes` for non-TTY stdin.
//! 9. Execute via [`vimprover::execute::run_recipe`].

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use vimprover::assess;
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
        FlowFlags {
            dry_run: args.dry_run,
            overwrite: args.overwrite,
            force: args.force,
            yes: args.yes,
        },
    )
    .await
}

/// Bundle of per-invocation flow-control flags so `run_single_file` doesn't
/// take a parameter list a mile long.
struct FlowFlags {
    dry_run: bool,
    overwrite: bool,
    force: bool,
    yes: bool,
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
    flags: FlowFlags,
) -> Result<()> {
    // 1. Probe.
    let profile: MediaProfile = probe::probe_file(input)
        .await
        .with_context(|| format!("probing {}", input.display()))?;
    println!("{}", format::render_profile(input, &profile));

    // 2. Assess + render the Issues block (only when there are issues).
    let assessment = assess::assess(&profile);
    if let Some(issues_block) = format::render_assessment(&assessment) {
        println!();
        println!("{issues_block}");
    }

    // 3. Fine-gate. Only `Intent::Auto` is gated; explicit intents
    //    (Reencode, Remux, etc.) bypass this check entirely.
    if matches!(intent, Intent::Auto) && assessment.is_fine() && !flags.force {
        println!();
        bail!(
            "{} is already fine — pass --force to process anyway, \
             or use --reencode / --intent for an explicit action.",
            input.display()
        );
    }

    // 4. Plan.
    let recipe = plan::plan(&profile, &assessment, &intent, &overrides)
        .with_context(|| "planning")?;

    // 5. Resolve output path + render the plan.
    let output = plan::resolve_output_path(user_output, &recipe);
    println!();
    println!("{}", format::render_recipe(&recipe, &output));

    // 6. Dry-run short-circuit: render the exact ffmpeg command and exit.
    if flags.dry_run {
        let ffmpeg = execute::locate_ffmpeg()?;
        let argv = execute::build_ffmpeg_args(
            &[input],
            std::slice::from_ref(&profile.container),
            &output,
            &recipe,
            flags.overwrite,
        );
        println!();
        println!("Command:   {}", execute::render_command(&ffmpeg, &argv));
        println!();
        println!("(dry run — not executing)");
        return Ok(());
    }

    // 7. Confirmation prompt (unless --yes).
    if !flags.yes && !confirm_proceed()? {
        println!("Aborted.");
        return Ok(());
    }

    // 8. Execute.
    println!();
    println!("Running ffmpeg…");
    execute::run_recipe(
        &[input],
        std::slice::from_ref(&profile.container),
        &output,
        &recipe,
        flags.overwrite,
    )
    .await
    .with_context(|| format!("encoding to {}", output.display()))?;

    println!("Done. Wrote {}.", output.display());
    Ok(())
}

/// Show a `[Y/n]` prompt and return whether the user accepted.
///
/// - Default (empty input) is "yes".
/// - Non-TTY stdin returns an error: batch scripts must pass `--yes`
///   explicitly so the user opts into non-interactive runs.
/// - Any input starting with `y`/`Y` is yes; `n`/`N` is no; everything
///   else falls back to the default.
fn confirm_proceed() -> Result<bool> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        bail!(
            "stdin is not a terminal; pass --yes to skip the confirmation prompt \
             in non-interactive runs"
        );
    }

    print!("\nProceed? [Y/n] ");
    std::io::stdout().flush().ok();

    let mut buf = String::new();
    stdin
        .lock()
        .read_line(&mut buf)
        .context("reading confirmation from stdin")?;

    Ok(match buf.trim().chars().next() {
        None => true,                 // bare Enter = default yes
        Some('y' | 'Y') => true,
        Some('n' | 'N') => false,
        _ => true,                    // anything else: default yes
    })
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
