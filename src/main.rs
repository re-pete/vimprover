//! `vimprover` binary entry point.
//!
//! Single-file flow:
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
//!
//! Concat flow (≥2 inputs, build-order step 6):
//!
//! 1. Parse CLI args.
//! 2. Probe each input in order → print the per-input profile.
//! 3. [`vimprover::plan::plan_concat`] checks stream uniformity; on mismatch
//!    bails with a precise error pointing at the offending input.
//! 4. Render the concat plan with [`vimprover::format::render_concat_recipe`].
//! 5. Dry-run / confirmation prompt as in single-file flow.
//! 6. Execute via [`vimprover::execute::run_recipe`] (which writes the
//!    concat-demuxer list file under the hood).
//!
//! Upgrade flow (`--upgrade`, single input, no explicit output):
//!
//! 1. Parse CLI args; reject multi-input or explicit-output combinations.
//! 2. Same probe/assess/plan as the single-file flow.
//! 3. [`vimprover::plan::resolve_upgrade_paths`] decides where the encoded
//!    output goes (next to the input, with the recipe's container ext) and
//!    whether a backup rename is needed (only if the output would collide
//!    with the input — i.e. same container).
//! 4. Render plan with [`vimprover::format::render_upgrade_recipe`], which
//!    notes either "original preserved unchanged" or "original will be
//!    renamed to …".
//! 5. Dry-run / confirmation prompt as usual.
//! 6. If a backup is needed, rename input → backup *before* spawning ffmpeg.
//!    On any encode failure, attempt a best-effort rollback of that rename.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

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

    let flags = FlowFlags {
        dry_run: args.dry_run,
        overwrite: args.overwrite,
        force: args.force,
        yes: args.yes,
    };

    // --upgrade short-circuit: takes exactly one INPUT, no OUTPUT positional.
    // Output path is auto-computed from the input + the recipe's container.
    if args.upgrade {
        if args.paths.len() != 1 {
            bail!(
                "--upgrade takes exactly one INPUT (got {}); \
                 for concat or explicit-output runs, omit --upgrade",
                args.paths.len()
            );
        }
        let intent = intent_from_args(&args);
        if matches!(intent, Intent::Concat) {
            bail!("--intent concat is incompatible with --upgrade");
        }
        let overrides = overrides_from_args(&args, None);
        return run_upgrade(&args.paths[0], intent, overrides, flags).await;
    }

    if args.paths.len() < 2 {
        bail!(
            "expected `INPUT [INPUT...] OUTPUT` or `--upgrade INPUT` (got {} path{})",
            args.paths.len(),
            if args.paths.len() == 1 { "" } else { "s" }
        );
    }

    let mut paths = args.paths.clone();
    let user_output = paths.pop().expect("checked len >= 2");
    let inputs = paths;

    let intent = intent_from_args(&args);
    let overrides = overrides_from_args(&args, Some(&user_output));

    // Multi-input ⇒ concat flow. Single-input + Intent::Concat is rejected
    // upstream by the planner via `Error::ConcatTooFewInputs`.
    if inputs.len() >= 2 {
        // Reject explicit non-concat intents that don't make sense with
        // multiple inputs. (Auto is fine — it implies concat.)
        match intent {
            Intent::Auto | Intent::Concat => {}
            Intent::Remux | Intent::Reencode | Intent::Shrink { .. } => bail!(
                "intent {:?} doesn't make sense with multiple inputs; \
                 omit --intent or use --intent concat",
                intent
            ),
        }
        return run_concat(&inputs, &user_output, overrides, flags).await;
    }

    if matches!(intent, Intent::Concat) {
        bail!(
            "--intent concat needs at least two inputs (got 1); \
             pass each input as a positional argument before the output path"
        );
    }

    run_single_file(&inputs[0], &user_output, intent, overrides, flags).await
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
    // Explicit --intent wins. --reencode is a back-compat shortcut for
    // --intent reencode (clap rejects mixing them, so we don't have to
    // resolve a conflict here).
    if let Some(cli_intent) = args.intent {
        return match cli_intent {
            cli::CliIntent::Auto => Intent::Auto,
            cli::CliIntent::Remux => Intent::Remux,
            cli::CliIntent::Reencode => Intent::Reencode,
            cli::CliIntent::Shrink => Intent::Shrink {
                max_height: args.max_height,
                target_bitrate_bps: args.target_bitrate,
            },
            cli::CliIntent::Concat => Intent::Concat,
        };
    }

    // No --intent: --reencode forces Reencode, otherwise Auto. If the user
    // passed --max-height or --target-bitrate without --intent shrink, treat
    // that as an implicit shrink request (those flags only make sense for
    // shrink, so accepting them as opt-in is the friendly thing to do).
    if args.reencode {
        Intent::Reencode
    } else if args.max_height.is_some() || args.target_bitrate.is_some() {
        Intent::Shrink {
            max_height: args.max_height,
            target_bitrate_bps: args.target_bitrate,
        }
    } else {
        Intent::Auto
    }
}

fn overrides_from_args(args: &Args, output_path: Option<&Path>) -> Overrides {
    // Explicit --container wins; otherwise infer from a recognized extension
    // on the output path (e.g. `out.mp4` → MP4). Per CLAUDE.md: "OUTPUT_NAME
    // without extension lets the planner pick the container. With extension
    // forces it." For `--upgrade` there's no user-supplied output path, so
    // only the explicit flag (if any) drives the container.
    let container = args
        .container
        .map(Into::into)
        .or_else(|| output_path.and_then(plan::container_from_output_extension));

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
    let assessment = assess::assess(&profile, &intent);
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
            None, // single-file: no concat list
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
    let input_size = std::fs::metadata(input).ok().map(|m| m.len());
    let started = Instant::now();
    execute::run_recipe(
        &[input],
        std::slice::from_ref(&profile.container),
        &output,
        &recipe,
        flags.overwrite,
        &profile
    )
    .await
    .with_context(|| format!("encoding to {}", output.display()))?;
    let elapsed = started.elapsed();
    let output_size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);

    println!(
        "{}",
        format::render_completion_summary(&output, input_size, output_size, elapsed)
    );
    Ok(())
}

/// Multi-input concat flow. Phase 1: demuxer-only stream-copy concat with a
/// strict uniformity check.
///
/// Probes each input in order, hands the slice to [`plan::plan_concat`], and
/// — if uniform — runs the same dry-run / prompt / execute path the single-
/// file flow uses.
async fn run_concat(
    inputs: &[PathBuf],
    user_output: &Path,
    overrides: Overrides,
    flags: FlowFlags,
) -> Result<()> {
    // 1. Probe every input. We print each profile so the user can eyeball
    //    the inputs before any work happens.
    let mut profiles: Vec<MediaProfile> = Vec::with_capacity(inputs.len());
    for (i, path) in inputs.iter().enumerate() {
        let profile = probe::probe_file(path)
            .await
            .with_context(|| format!("probing input #{} ({})", i + 1, path.display()))?;
        if i > 0 {
            println!();
        }
        println!("{}", format::render_profile(path, &profile));
        profiles.push(profile);
    }

    // 2. Plan: uniformity check + recipe. Errors here carry the "normalize
    //    first" hint already.
    let input_refs: Vec<&Path> = inputs.iter().map(PathBuf::as_path).collect();
    let recipe = plan::plan_concat(&input_refs, &profiles, &overrides)
        .with_context(|| "planning concat")?;

    // 3. Resolve output path + render the plan.
    let output = plan::resolve_output_path(user_output, &recipe);
    println!();
    println!(
        "{}",
        format::render_concat_recipe(&recipe, &input_refs, &output)
    );

    // 4. Dry-run short-circuit. Build the same argv the run will use, with
    //    a placeholder list-file path so the user can see the shape of the
    //    command. (The real list file is written by run_recipe at exec time.)
    if flags.dry_run {
        let ffmpeg = execute::locate_ffmpeg()?;
        let placeholder = std::path::Path::new("<concat-list-tempfile>");
        // Source containers: we still need the slice for the API, even
        // though concat-mode skips per-input genpts.
        let containers: Vec<_> = profiles.iter().map(|p| p.container.clone()).collect();
        let argv = execute::build_ffmpeg_args(
            &input_refs,
            &containers,
            &output,
            &recipe,
            flags.overwrite,
            Some(placeholder),
        );
        println!();
        println!("Command:   {}", execute::render_command(&ffmpeg, &argv));
        println!();
        println!("(dry run — not executing; the list file is created at run time)");
        return Ok(());
    }

    // 5. Confirmation prompt.
    if !flags.yes && !confirm_proceed()? {
        println!("Aborted.");
        return Ok(());
    }

    // 6. Execute.
    println!();
    println!("Running ffmpeg…");
    let containers: Vec<_> = profiles.iter().map(|p| p.container.clone()).collect();
    let total_input_size = sum_input_sizes(inputs);
    let started = Instant::now();
    execute::run_recipe(
        &input_refs,
        &containers,
        &output,
        &recipe,
        flags.overwrite,
        &profiles[0],
    )
    .await
    .with_context(|| format!("encoding to {}", output.display()))?;
    let elapsed = started.elapsed();
    let output_size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);

    println!(
        "{}",
        format::render_concat_completion_summary(
            &output,
            inputs.len(),
            total_input_size,
            output_size,
            elapsed,
        )
    );
    Ok(())
}

/// Single-input `--upgrade` flow. Same probe/assess/plan as
/// [`run_single_file`], but the output path is computed from the input
/// (placed alongside it with the recipe's container extension) and a
/// backup-rename dance protects the original on a same-container upgrade.
async fn run_upgrade(
    input: &Path,
    intent: Intent,
    overrides: Overrides,
    flags: FlowFlags,
) -> Result<()> {
    // 1. Probe.
    let profile: MediaProfile = probe::probe_file(input)
        .await
        .with_context(|| format!("probing {}", input.display()))?;
    println!("{}", format::render_profile(input, &profile));

    // 2. Assess.
    let assessment = assess::assess(&profile, &intent);
    if let Some(issues_block) = format::render_assessment(&assessment) {
        println!();
        println!("{issues_block}");
    }

    // 3. Fine-gate (Auto only).
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

    // 5. Resolve upgrade paths.
    let paths = plan::resolve_upgrade_paths(input, &recipe);

    // 6. Render the plan with the upgrade caveat.
    println!();
    println!(
        "{}",
        format::render_upgrade_recipe(&recipe, input, &paths.output, paths.backup.as_deref())
    );

    // 7. Pre-flight: refuse if the output (when it's a different file from
    //    the input) or the backup already exists, unless --overwrite.
    if !flags.overwrite {
        if paths.output != input && paths.output.exists() {
            bail!(
                "output already exists: {} (pass --overwrite to replace)",
                paths.output.display()
            );
        }
        if let Some(ref backup) = paths.backup {
            if backup.exists() {
                bail!(
                    "backup file from a prior upgrade exists: {} \
                     (pass --overwrite to replace, or remove it manually)",
                    backup.display()
                );
            }
        }
    }

    // 8. Dry-run short-circuit. We render the argv against the *current*
    //    input path even in the same-container case — the actual run will
    //    read from the backup path after the rename, but dry-run is a
    //    preview and using the visible path here is more readable.
    if flags.dry_run {
        let ffmpeg = execute::locate_ffmpeg()?;
        let argv = execute::build_ffmpeg_args(
            &[input],
            std::slice::from_ref(&profile.container),
            &paths.output,
            &recipe,
            flags.overwrite,
            None,
        );
        println!();
        println!("Command:   {}", execute::render_command(&ffmpeg, &argv));
        println!();
        println!("(dry run — not executing)");
        return Ok(());
    }

    // 9. Confirmation prompt.
    if !flags.yes && !confirm_proceed()? {
        println!("Aborted.");
        return Ok(());
    }

    // 10. Execute. Backup-then-encode dance with rollback on failure.
    println!();
    println!("Running ffmpeg…");
    let input_size = std::fs::metadata(input).ok().map(|m| m.len());
    let started = Instant::now();

    // Clear stale backup if --overwrite is set (the user opted in above).
    if flags.overwrite {
        if let Some(ref backup) = paths.backup {
            let _ = std::fs::remove_file(backup);
        }
    }

    // Step A: backup rename (only if needed).
    if let Some(ref backup) = paths.backup {
        std::fs::rename(input, backup).with_context(|| {
            format!(
                "renaming original to backup ({} → {})",
                input.display(),
                backup.display()
            )
        })?;
    }

    // Step B: encode. In the same-container case the input now lives at
    //         the backup path; otherwise it's still at its original path.
    let encode_input = paths.backup.as_deref().unwrap_or(input);
    let encode_result = execute::run_recipe(
        &[encode_input],
        std::slice::from_ref(&profile.container),
        &paths.output,
        &recipe,
        flags.overwrite,
        &profile,
    )
    .await;

    // Step C: handle failure with best-effort rollback of the backup rename.
    if let Err(e) = encode_result {
        if let Some(ref backup) = paths.backup {
            if let Err(restore_err) = std::fs::rename(backup, input) {
                eprintln!(
                    "warning: upgrade failed and could not restore backup {} to {}: {restore_err}",
                    backup.display(),
                    input.display(),
                );
                eprintln!("the original file is preserved at: {}", backup.display());
            }
        }
        return Err(e).with_context(|| format!("encoding to {}", paths.output.display()));
    }

    let elapsed = started.elapsed();
    let output_size = std::fs::metadata(&paths.output).map(|m| m.len()).unwrap_or(0);

    println!(
        "{}",
        format::render_completion_summary(&paths.output, input_size, output_size, elapsed)
    );
    if let Some(ref backup) = paths.backup {
        println!("Original preserved at: {}", backup.display());
    }
    Ok(())
}

/// Sum the on-disk sizes of all inputs. Returns `None` if any single stat
/// fails (rather than reporting a misleading partial total).
fn sum_input_sizes(inputs: &[PathBuf]) -> Option<u64> {
    let mut total: u64 = 0;
    for p in inputs {
        let size = std::fs::metadata(p).ok()?.len();
        total = total.checked_add(size)?;
    }
    Some(total)
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
