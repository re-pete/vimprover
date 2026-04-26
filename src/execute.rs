//! Step 5 of the pipeline: build ffmpeg argv and run it.
//!
//! In build-order step 2 only the stream-copy remux path is exercised, but the
//! arg-builder handles every [`VideoStrategy`] / [`AudioStrategy`] / [`VideoFilter`]
//! defined in [`crate::plan`] so future steps drop in cleanly.
//!
//! Implementation notes from `CLAUDE.md`:
//!
//! - Build args as `Vec<OsString>`; never shell-string concat (path injection risk).
//! - Don't use `Command::output()` for long-running processes — we inherit
//!   ffmpeg's stderr so the user sees progress/errors live.
//! - Capture stderr-style failures via the exit status; ffmpeg has already
//!   printed its diagnostic to the terminal.
//! - Support multi-step pipelines for concat mode (deferred to step 6).
//! - Log exact ffmpeg commands at info level via [`render_command`] so users
//!   can paste them into a terminal to reproduce.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;
use tracing::{debug, info};

use crate::error::{Error, Result};
use crate::model::Container;
use crate::plan::{AudioStrategy, EncodeRecipe, VideoFilter, VideoStrategy};

/// Locate the ffmpeg binary, preferring the `VIMPROVER_FFMPEG` env override.
pub fn locate_ffmpeg() -> Result<PathBuf> {
    if let Ok(override_path) = env::var("VIMPROVER_FFMPEG") {
        if !override_path.is_empty() {
            return Ok(PathBuf::from(override_path));
        }
    }
    which::which("ffmpeg").map_err(Error::FfmpegNotFound)
}

/// Build the full ffmpeg argv for `recipe`.
///
/// `inputs` and `source_containers` must be the same length. The `i`-th entry
/// of `source_containers` is the container of `inputs[i]` (used to apply
/// per-input flags such as `-fflags +genpts` for MPEG program/transport streams).
///
/// In step 2, exactly one input is supported. Multi-input concat lands in
/// step 6.
pub fn build_ffmpeg_args(
    inputs: &[&Path],
    source_containers: &[Container],
    output: &Path,
    recipe: &EncodeRecipe,
    overwrite: bool,
) -> Vec<OsString> {
    assert_eq!(
        inputs.len(),
        source_containers.len(),
        "build_ffmpeg_args: inputs and source_containers must align"
    );

    let mut args: Vec<OsString> = Vec::with_capacity(32 + recipe.extra_flags.len());

    args.push("-hide_banner".into());
    args.push("-loglevel".into());
    args.push("warning".into());
    args.push("-stats".into());
    args.push(if overwrite { "-y" } else { "-n" }.into());

    for (path, container) in inputs.iter().zip(source_containers) {
        // Per-input flags. `+genpts` asks the demuxer to regenerate missing
        // PTS from DTS, which MP4/MKV/MOV muxers require. Needed for:
        // - MPEG-PS/TS (VOB, transport streams) — classical NOPTS sources.
        // - AVI carrying MPEG-4 ASP (Xvid/DivX) with B-frames: AVI has no
        //   per-packet PTS, and the demuxer cannot infer display-order
        //   timestamps for reordered frames without help.
        if needs_genpts(container) {
            args.push("-fflags".into());
            args.push("+genpts".into());
        }
        args.push("-i".into());
        args.push((*path).as_os_str().into());
    }

    // Stream selection. Single-input only for step 2.
    if inputs.len() == 1 {
        for s in ["-map", "0:v:0", "-map", "0:a?", "-map", "0:s?"] {
            args.push(s.into());
        }
    }

    // Video.
    match &recipe.video_strategy {
        VideoStrategy::Copy => push_pair(&mut args, "-c:v", "copy"),
        VideoStrategy::ReencodeX264 { crf, preset } => {
            push_pair(&mut args, "-c:v", "libx264");
            push_pair(&mut args, "-crf", &crf.to_string());
            push_pair(&mut args, "-preset", preset);
        }
        VideoStrategy::ReencodeX265 { crf, preset } => {
            push_pair(&mut args, "-c:v", "libx265");
            push_pair(&mut args, "-crf", &crf.to_string());
            push_pair(&mut args, "-preset", preset);
        }
        VideoStrategy::ReencodeX264Abr { target_bps, max_bps, bufsize_bps, preset } => {
            push_pair(&mut args, "-c:v", "libx264");
            push_pair(&mut args, "-b:v", target_bps.to_string().as_str());
            push_pair(&mut args, "-maxrate", max_bps.to_string().as_str());
            push_pair(&mut args, "-bufsize", bufsize_bps.to_string().as_str());
            push_pair(&mut args, "-preset", preset);
        }
        VideoStrategy::ReencodeX265Abr { target_bps, max_bps, bufsize_bps, preset } => {
            push_pair(&mut args, "-c:v", "libx265");
            push_pair(&mut args, "-b:v", target_bps.to_string().as_str());
            push_pair(&mut args, "-maxrate", max_bps.to_string().as_str());
            push_pair(&mut args, "-bufsize", bufsize_bps.to_string().as_str());
            push_pair(&mut args, "-preset", preset);
        }
    }

    // Filters.
    if !recipe.video_filters.is_empty() {
        args.push("-vf".into());
        args.push(render_video_filter_chain(&recipe.video_filters).into());
    }

    // Audio.
    match &recipe.audio_strategy {
        AudioStrategy::Copy => push_pair(&mut args, "-c:a", "copy"),
        AudioStrategy::AacStereo { bitrate_bps } => {
            push_pair(&mut args, "-c:a", "aac");
            push_pair(&mut args, "-ac", "2");
            push_pair(&mut args, "-b:a", &format_audio_bitrate(*bitrate_bps));
        }
        AudioStrategy::AacMultichannel { bitrate_bps } => {
            push_pair(&mut args, "-c:a", "aac");
            push_pair(&mut args, "-b:a", &format_audio_bitrate(*bitrate_bps));
        }
    }

    // Subtitles always stream-copy when present (`-map 0:s?` already gates
    // the absence-case).
    push_pair(&mut args, "-c:s", "copy");

    // Extra recipe flags (e.g. -movflags +faststart).
    for flag in &recipe.extra_flags {
        args.push(flag.into());
    }

    args.push(output.as_os_str().into());
    args
}

fn push_pair(args: &mut Vec<OsString>, k: &str, v: &str) {
    args.push(k.into());
    args.push(v.into());
}

/// Containers whose demuxers commonly emit packets with missing PTS, requiring
/// `-fflags +genpts` to produce a valid MP4/MKV output.
///
/// - **MPEG-PS / MPEG-TS**: classical NOPTS sources (VOBs, transport streams).
/// - **AVI**: stores frame-index timing with no per-packet PTS; reordered
///   MPEG-4 ASP (Xvid/DivX) with B-frames is the most common failure mode.
fn needs_genpts(container: &Container) -> bool {
    matches!(
        container,
        Container::MpegPs | Container::MpegTs | Container::Avi
    )
}

fn render_video_filter_chain(filters: &[VideoFilter]) -> String {
    filters
        .iter()
        .map(|f| match f {
            VideoFilter::Bwdif => "bwdif=1".to_string(),
            VideoFilter::FieldmatchDecimate => "fieldmatch,decimate".to_string(),
            VideoFilter::Scale { width, height } => format!("scale={width}:{height}"),
            VideoFilter::SetSar { num, den } => format!("setsar={num}/{den}"),
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn format_audio_bitrate(bps: u64) -> String {
    if bps % 1000 == 0 {
        format!("{}k", bps / 1000)
    } else {
        bps.to_string()
    }
}

/// Render an argv as a single shell-pasteable string for debug logging.
///
/// Quoting is conservative: any token containing whitespace or shell
/// metacharacters is single-quoted, with embedded single quotes escaped.
pub fn render_command(binary: &Path, args: &[OsString]) -> String {
    let mut out = String::new();
    push_shell_escaped(&mut out, &binary.to_string_lossy());
    for a in args {
        out.push(' ');
        push_shell_escaped(&mut out, &a.to_string_lossy());
    }
    out
}

fn push_shell_escaped(out: &mut String, s: &str) {
    let needs_quoting = s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || "'\"\\$`&|;<>()#?*[]{}!~".contains(c));
    if !needs_quoting {
        out.push_str(s);
        return;
    }
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
}

/// Spawn ffmpeg according to `recipe`. `inherit`s stdio so the user sees
/// ffmpeg's progress and errors live; on non-zero exit returns
/// [`Error::FfmpegFailed`].
pub async fn run_recipe(
    inputs: &[&Path],
    source_containers: &[Container],
    output: &Path,
    recipe: &EncodeRecipe,
    overwrite: bool,
) -> Result<()> {
    if !overwrite && output.exists() {
        return Err(Error::OutputExists(output.to_path_buf()));
    }

    let ffmpeg = locate_ffmpeg()?;
    let args = build_ffmpeg_args(inputs, source_containers, output, recipe, overwrite);
    let cmd_str = render_command(&ffmpeg, &args);
    info!(cmd = %cmd_str, "running ffmpeg");
    debug!(?ffmpeg, ?args, "spawning ffmpeg");

    let status = Command::new(&ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if !status.success() {
        return Err(Error::FfmpegFailed {
            status: status.code().unwrap_or(-1),
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{EncodeRecipe, VideoStrategy};
    use pretty_assertions::assert_eq;

    fn remux_recipe_mkv() -> EncodeRecipe {
        EncodeRecipe {
            output_container: Container::Mkv,
            video_strategy: VideoStrategy::Copy,
            video_filters: Vec::new(),
            audio_strategy: AudioStrategy::Copy,
            extra_flags: Vec::new(),
        }
    }

    fn remux_recipe_mp4() -> EncodeRecipe {
        EncodeRecipe {
            output_container: Container::Mp4,
            video_strategy: VideoStrategy::Copy,
            video_filters: Vec::new(),
            audio_strategy: AudioStrategy::Copy,
            extra_flags: vec!["-movflags".into(), "+faststart".into()],
        }
    }

    fn args_as_str(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn vob_remux_mkv_argv() {
        let inputs: &[&Path] = &[Path::new("/tmp/movie.vob")];
        let containers = &[Container::MpegPs];
        let out = Path::new("/tmp/newname.mkv");
        let args = build_ffmpeg_args(inputs, containers, out, &remux_recipe_mkv(), false);

        assert_eq!(
            args_as_str(&args),
            vec![
                "-hide_banner",
                "-loglevel",
                "warning",
                "-stats",
                "-n",
                "-fflags",
                "+genpts",
                "-i",
                "/tmp/movie.vob",
                "-map",
                "0:v:0",
                "-map",
                "0:a?",
                "-map",
                "0:s?",
                "-c:v",
                "copy",
                "-c:a",
                "copy",
                "-c:s",
                "copy",
                "/tmp/newname.mkv",
            ]
        );
    }

    #[test]
    fn avi_input_gets_genpts() {
        // Regression: AVI carrying MPEG-4 ASP with B-frames fails to remux
        // into MKV without `-fflags +genpts` (the AVI demuxer emits packets
        // without PTS and the MKV muxer rejects them).
        let inputs: &[&Path] = &[Path::new("/tmp/movie.avi")];
        let containers = &[Container::Avi];
        let out = Path::new("/tmp/out.mkv");
        let args = build_ffmpeg_args(inputs, containers, out, &remux_recipe_mkv(), false);
        let strs = args_as_str(&args);

        let genpts_idx = strs
            .iter()
            .position(|s| s == "+genpts")
            .expect("AVI input should get -fflags +genpts");
        assert_eq!(strs[genpts_idx - 1], "-fflags");
        // The flag must appear before the -i that introduces the AVI input.
        let input_idx = strs.iter().position(|s| s == "/tmp/movie.avi").unwrap();
        assert!(genpts_idx < input_idx);
    }

    #[test]
    fn modern_containers_skip_genpts() {
        // MP4/MKV/MOV/WebM all store per-packet timestamps; +genpts would be
        // a no-op but we keep the command minimal.
        for c in [
            Container::Mp4,
            Container::Mkv,
            Container::Mov,
            Container::WebM,
        ] {
            assert!(!needs_genpts(&c), "{c:?} should not request +genpts");
        }
        assert!(needs_genpts(&Container::MpegPs));
        assert!(needs_genpts(&Container::MpegTs));
        assert!(needs_genpts(&Container::Avi));
    }

    #[test]
    fn mp4_recipe_appends_faststart() {
        let inputs: &[&Path] = &[Path::new("/tmp/in.flv")];
        let containers = &[Container::Flv];
        let out = Path::new("/tmp/out.mp4");
        let args = build_ffmpeg_args(inputs, containers, out, &remux_recipe_mp4(), true);
        let strs = args_as_str(&args);

        // Overwrite mode uses -y, not -n.
        assert!(strs.contains(&"-y".into()));
        assert!(!strs.contains(&"-n".into()));
        // No genpts for FLV.
        assert!(!strs.contains(&"-fflags".into()));
        // Faststart flag is present, just before the output.
        let faststart_idx = strs.iter().position(|s| s == "+faststart").unwrap();
        assert_eq!(strs[faststart_idx - 1], "-movflags");
        assert_eq!(strs.last().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn render_command_quotes_paths_with_spaces() {
        let bin = PathBuf::from("/usr/bin/ffmpeg");
        let args: Vec<OsString> = vec![
            "-i".into(),
            "/tmp/movie with spaces.vob".into(),
            "-c".into(),
            "copy".into(),
            "/tmp/out's.mkv".into(),
        ];
        let cmd = render_command(&bin, &args);
        assert_eq!(
            cmd,
            r"/usr/bin/ffmpeg -i '/tmp/movie with spaces.vob' -c copy '/tmp/out'\''s.mkv'"
        );
    }

    #[test]
    fn formats_audio_bitrate_kbps() {
        assert_eq!(format_audio_bitrate(192_000), "192k");
        assert_eq!(format_audio_bitrate(128_000), "128k");
        assert_eq!(format_audio_bitrate(96_500), "96500");
    }

    #[test]
    fn renders_filter_chain() {
        let chain = render_video_filter_chain(&[
            VideoFilter::Bwdif,
            VideoFilter::Scale { width: 1920, height: 1080 },
            VideoFilter::SetSar { num: 1, den: 1 },
        ]);
        assert_eq!(chain, "bwdif=1,scale=1920:1080,setsar=1/1");
    }
}
