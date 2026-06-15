//! Step 5 of the pipeline: build ffmpeg argv and run it.
//!
//! Handles every [`VideoStrategy`] / [`AudioStrategy`] / [`VideoFilter`] from
//! [`crate::plan`], plus (as of step 6) the concat-demuxer path for
//! multi-input stream-copy concatenation.
//!
//! Implementation notes from `CLAUDE.md`:
//!
//! - Build args as `Vec<OsString>`; never shell-string concat (path injection risk).
//! - Don't use `Command::output()` for long-running processes — we inherit
//!   ffmpeg's stderr so the user sees progress/errors live.
//! - Capture stderr-style failures via the exit status; ffmpeg has already
//!   printed its diagnostic to the terminal.
//! - Concat mode uses ffmpeg's `-f concat -safe 0 -i LIST.txt` demuxer; the
//!   list file is a short-lived tempfile with properly-escaped paths.
//! - Log exact ffmpeg commands at info level via [`render_command`] so users
//!   can paste them into a terminal to reproduce.

use std::env;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tempfile::NamedTempFile;
use tokio::process::Command;
use tracing::{debug, info};

use crate::error::{Error, Result};
use crate::model::Container;
use crate::plan::{AudioStrategy, ConcatStrategy, EncodeRecipe, VideoFilter, VideoStrategy};

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
/// Two input modes are supported:
///
/// - **Single-file / multi-separate-input**: pass each `input[i]` as its own
///   `-i` arg. Triggered when `recipe.concat` is `None`.
/// - **Concat demuxer**: pass one virtual input via `-f concat -safe 0 -i
///   LIST`. Triggered when `recipe.concat == Some(ConcatStrategy::Demuxer)`.
///   `concat_list_file` must be `Some(list_path)` in this case; the caller
///   (typically [`run_recipe`]) writes the list file. In concat mode,
///   per-input `-fflags +genpts` is skipped — the demuxer handles timestamps
///   across segments when stream-copying uniform inputs.
pub fn build_ffmpeg_args(
    inputs: &[&Path],
    source_containers: &[Container],
    output: &Path,
    recipe: &EncodeRecipe,
    overwrite: bool,
    concat_list_file: Option<&Path>,
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
    args.push(if overwrite { "-y" } else { "-n" }.into());

    match (recipe.concat, concat_list_file) {
        (Some(ConcatStrategy::Demuxer), Some(list_path)) => {
            // Concat demuxer: one virtual input, stream-copy across segments.
            // `-safe 0` is required because our list-file paths are absolute
            // and may contain characters ffmpeg's default safety check
            // rejects. `+genpts` isn't needed: with uniform stream-copy the
            // demuxer concatenates frame timestamps cleanly.
            args.push("-f".into());
            args.push("concat".into());
            args.push("-safe".into());
            args.push("0".into());
            args.push("-i".into());
            args.push(list_path.as_os_str().into());
        }
        (Some(ConcatStrategy::Demuxer), None) => {
            // Programmer error: concat recipe but no list file provided.
            // Fall through to the per-input -i path; the caller's tests
            // will surface the bug.
            for (path, container) in inputs.iter().zip(source_containers) {
                if needs_genpts(container) {
                    args.push("-fflags".into());
                    args.push("+genpts".into());
                }
                args.push("-i".into());
                args.push((*path).as_os_str().into());
            }
        }
        (None, _) => {
            for (path, container) in inputs.iter().zip(source_containers) {
                // Per-input flags. `+genpts` asks the demuxer to regenerate
                // missing PTS from DTS, which MP4/MKV/MOV muxers require.
                // Needed for:
                // - MPEG-PS/TS (VOB, transport streams) — classical NOPTS.
                // - AVI carrying MPEG-4 ASP (Xvid/DivX) with B-frames: AVI
                //   has no per-packet PTS, and the demuxer cannot infer
                //   display-order timestamps for reordered frames without
                //   help.
                if needs_genpts(container) {
                    args.push("-fflags".into());
                    args.push("+genpts".into());
                }
                args.push("-i".into());
                args.push((*path).as_os_str().into());
            }
        }
    }

    // Stream selection. In both single-file and concat-demuxer modes there
    // is exactly one virtual input 0, so the map specification is identical.
    let select_streams =
        recipe.concat.is_some() || inputs.len() == 1;
    if select_streams {
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
///
/// **Atomic output.** ffmpeg writes to a `<output-stem>.partial.<ext>`
/// sibling of the requested `output`. On success we [`std::fs::rename`] it
/// into place (atomic on the same filesystem). On failure the partial is
/// left behind so the user can inspect it; the next run with the same
/// arguments will refuse it (or replace it with `--overwrite`). This means
/// users never see a half-written file at the real output path, even if
/// ffmpeg crashes or is Ctrl-C'd halfway through.
///
/// When `recipe.concat == Some(Demuxer)`, writes a temp list file for the
/// concat demuxer (auto-deleted when ffmpeg exits — success or failure)
/// before spawning.
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

    // Compute the partial-output sibling path. ffmpeg writes here; we
    // rename to `output` on success.
    let partial = partial_sibling(output);
    if !overwrite && partial.exists() {
        return Err(Error::PartialOutputExists(partial));
    }
    if overwrite {
        // Clean up any stale partial so ffmpeg's own `-y` overwrite check
        // doesn't matter (we're using a fresh path either way).
        let _ = std::fs::remove_file(&partial);
    }

    // Keep the list-file handle alive for the duration of the ffmpeg run; it
    // auto-deletes on drop.
    let concat_list = if matches!(recipe.concat, Some(ConcatStrategy::Demuxer)) {
        Some(write_concat_list(inputs)?)
    } else {
        None
    };
    let list_path = concat_list.as_ref().map(|f| f.path());

    let ffmpeg = locate_ffmpeg()?;
    let args = build_ffmpeg_args(
        inputs,
        source_containers,
        &partial,
        recipe,
        overwrite,
        list_path,
    );
    let cmd_str = render_command(&ffmpeg, &args);
    info!(cmd = %cmd_str, "running ffmpeg");
    debug!(?ffmpeg, ?args, "spawning ffmpeg");

    let mut child = Command::new(&ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;



    let status = child.wait().await?;
    if !status.success() {
        // Leave the partial file in place for inspection / debugging.
        return Err(Error::FfmpegFailed {
            status: status.code().unwrap_or(-1),
        });
    }

    // ffmpeg succeeded — atomically promote the partial to the real path.
    std::fs::rename(&partial, output).map_err(|source| Error::RenameFailed {
        from: partial.clone(),
        to: output.to_path_buf(),
        source,
    })?;

    // `concat_list` drops here, unlinking the temp file.
    drop(concat_list);
    Ok(())
}

/// Compute the partial-output sibling path for `output`.
///
/// Inserts `.partial` between the file stem and the extension so the
/// resulting filename keeps an extension ffmpeg's muxer-by-extension
/// inference understands:
///
/// - `/path/to/foo.mkv`  →  `/path/to/foo.partial.mkv`
/// - `/path/to/foo`      →  `/path/to/foo.partial` (no extension to preserve)
/// - `foo.mkv`           →  `foo.partial.mkv`
///
/// The placement-before-extension matters: if we appended `.partial` after
/// the extension (`foo.mkv.partial`), ffmpeg would refuse to write because
/// it can't infer the muxer from `.partial`. Rewriting in-place lets us
/// keep ffmpeg's argv unchanged from the single-target case.
pub fn partial_sibling(output: &Path) -> PathBuf {
    let parent = output.parent();
    let stem = output.file_stem();
    let ext = output.extension();

    let mut name = OsString::new();
    if let Some(stem) = stem {
        name.push(stem);
    }
    name.push(".partial");
    if let Some(ext) = ext {
        name.push(".");
        name.push(ext);
    }

    match parent {
        Some(p) if !p.as_os_str().is_empty() => p.join(name),
        _ => PathBuf::from(name),
    }
}

/// Write a tempfile containing the ffmpeg concat-demuxer list format, one
/// `file '<path>'` directive per input.
///
/// **Absolute-path normalization** (critical): ffmpeg's concat demuxer
/// resolves relative paths in the list file *relative to the list file's
/// directory*, not the caller's CWD. Since our list file lives in the
/// system temp directory, a relative input like `fo1.mkv` would be looked
/// up as `/tmp/fo1.mkv` and fail. We therefore convert every input to an
/// absolute path here via [`std::path::absolute`] (lexical; no filesystem
/// access, no symlink following) before writing it out.
///
/// **Quoting** follows ffmpeg's documented rules for the concat demuxer:
/// paths are wrapped in single quotes; any embedded `'` is escaped as
/// `'\''` (close-quote, literal-quote, open-quote). Backslashes are NOT
/// special — they pass through literally, so Windows-style paths work.
///
/// The caller owns the returned `NamedTempFile` and must keep it alive for
/// as long as ffmpeg needs to read the list (typically: spawn → wait).
pub fn write_concat_list(inputs: &[&Path]) -> Result<NamedTempFile> {
    let mut file = NamedTempFile::with_prefix("vimprover-concat-")?;
    for input in inputs {
        let absolute = std::path::absolute(input)?;
        let line = format!("file '{}'\n", escape_concat_list_path(&absolute));
        file.write_all(line.as_bytes())?;
    }
    file.flush()?;
    Ok(file)
}

/// Escape a path for ffmpeg's concat-demuxer list file.
///
/// Inside a single-quoted ffmpeg directive, only the single quote is special.
/// See <https://ffmpeg.org/ffmpeg-formats.html#concat>.
fn escape_concat_list_path(path: &Path) -> String {
    // `to_string_lossy` is fine here: concat's list-file parser reads bytes
    // as UTF-8. If the user's filesystem uses a non-UTF-8 encoding, ffmpeg
    // wouldn't be able to open the file via this list anyway.
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out
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
            concat: None,
        }
    }

    fn remux_recipe_mp4() -> EncodeRecipe {
        EncodeRecipe {
            output_container: Container::Mp4,
            video_strategy: VideoStrategy::Copy,
            video_filters: Vec::new(),
            audio_strategy: AudioStrategy::Copy,
            extra_flags: vec!["-movflags".into(), "+faststart".into()],
            concat: None,
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
        let args = build_ffmpeg_args(inputs, containers, out, &remux_recipe_mkv(), false, None);

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
        let args = build_ffmpeg_args(inputs, containers, out, &remux_recipe_mkv(), false, None);
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
        let args = build_ffmpeg_args(inputs, containers, out, &remux_recipe_mp4(), true, None);
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

    // -----------------------------------------------------------------------
    // Concat-demuxer mode (step 6)
    // -----------------------------------------------------------------------

    fn concat_recipe_mkv() -> EncodeRecipe {
        EncodeRecipe {
            output_container: Container::Mkv,
            video_strategy: VideoStrategy::Copy,
            video_filters: Vec::new(),
            audio_strategy: AudioStrategy::Copy,
            extra_flags: Vec::new(),
            concat: Some(ConcatStrategy::Demuxer),
        }
    }

    #[test]
    fn concat_argv_uses_concat_demuxer_with_list_file() {
        let inputs: &[&Path] = &[Path::new("/tmp/a.mp4"), Path::new("/tmp/b.mp4")];
        let containers = &[Container::Mp4, Container::Mp4];
        let out = Path::new("/tmp/joined.mkv");
        let list = Path::new("/tmp/concat-list.txt");

        let args = build_ffmpeg_args(
            inputs,
            containers,
            out,
            &concat_recipe_mkv(),
            false,
            Some(list),
        );

        assert_eq!(
            args_as_str(&args),
            vec![
                "-hide_banner",
                "-loglevel",
                "warning",
                "-stats",
                "-n",
                "-f",
                "concat",
                "-safe",
                "0",
                "-i",
                "/tmp/concat-list.txt",
                // single virtual input → same -map as single-file mode
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
                "/tmp/joined.mkv",
            ]
        );
    }

    #[test]
    fn concat_argv_skips_per_input_genpts_even_for_avi() {
        // Even if the inputs would normally trigger -fflags +genpts (e.g. AVI),
        // concat-demuxer mode skips them: timestamps are reconstructed from
        // the concatenated stream, not the per-input demuxers.
        let inputs: &[&Path] = &[Path::new("/tmp/a.avi"), Path::new("/tmp/b.avi")];
        let containers = &[Container::Avi, Container::Avi];
        let out = Path::new("/tmp/joined.mkv");
        let list = Path::new("/tmp/list.txt");

        let args = build_ffmpeg_args(
            inputs,
            containers,
            out,
            &concat_recipe_mkv(),
            false,
            Some(list),
        );
        let strs = args_as_str(&args);

        assert!(!strs.iter().any(|s| s == "-fflags"), "should not emit -fflags in concat mode: {strs:?}");
        assert!(!strs.iter().any(|s| s == "+genpts"), "should not emit +genpts in concat mode: {strs:?}");
    }

    #[test]
    fn concat_argv_passes_through_mp4_faststart() {
        let inputs: &[&Path] = &[Path::new("/tmp/a.mp4"), Path::new("/tmp/b.mp4")];
        let containers = &[Container::Mp4, Container::Mp4];
        let out = Path::new("/tmp/joined.mp4");
        let list = Path::new("/tmp/list.txt");

        let recipe = EncodeRecipe {
            output_container: Container::Mp4,
            video_strategy: VideoStrategy::Copy,
            video_filters: Vec::new(),
            audio_strategy: AudioStrategy::Copy,
            extra_flags: vec!["-movflags".into(), "+faststart".into()],
            concat: Some(ConcatStrategy::Demuxer),
        };
        let args = build_ffmpeg_args(inputs, containers, out, &recipe, false, Some(list));
        let strs = args_as_str(&args);

        let faststart_idx = strs.iter().position(|s| s == "+faststart").unwrap();
        assert_eq!(strs[faststart_idx - 1], "-movflags");
        assert_eq!(strs.last().unwrap(), "/tmp/joined.mp4");
    }

    #[test]
    fn escape_concat_list_path_passes_through_simple_paths() {
        assert_eq!(
            escape_concat_list_path(Path::new("/tmp/clip1.mp4")),
            "/tmp/clip1.mp4"
        );
    }

    #[test]
    fn escape_concat_list_path_escapes_single_quotes() {
        // 'foo' becomes '\'' (close, escaped quote, reopen) — that's three
        // chars -> four char output for one input quote.
        assert_eq!(
            escape_concat_list_path(Path::new("/tmp/it's.mp4")),
            r"/tmp/it'\''s.mp4"
        );
    }

    #[test]
    fn escape_concat_list_path_passes_spaces_and_backslashes() {
        // Spaces are fine inside single-quoted directives. Backslashes are
        // not special to ffmpeg's concat demuxer.
        assert_eq!(
            escape_concat_list_path(Path::new("/tmp/with spaces.mp4")),
            "/tmp/with spaces.mp4"
        );
        assert_eq!(
            escape_concat_list_path(Path::new(r"C:\videos\clip.mp4")),
            r"C:\videos\clip.mp4"
        );
    }

    #[test]
    fn write_concat_list_emits_file_directives() {
        // Absolute inputs are written verbatim (modulo quote escaping).
        let inputs: &[&Path] = &[
            Path::new("/tmp/a.mp4"),
            Path::new("/tmp/b's.mp4"),
            Path::new("/tmp/c with space.mp4"),
        ];
        let f = write_concat_list(inputs).expect("write list");
        let content = std::fs::read_to_string(f.path()).expect("read list");
        let expected = "\
file '/tmp/a.mp4'
file '/tmp/b'\\''s.mp4'
file '/tmp/c with space.mp4'
";
        assert_eq!(content, expected);
    }

    // -----------------------------------------------------------------------
    // partial_sibling
    // -----------------------------------------------------------------------

    #[test]
    fn partial_sibling_inserts_dot_partial_before_extension() {
        assert_eq!(
            partial_sibling(Path::new("/path/to/foo.mkv")),
            PathBuf::from("/path/to/foo.partial.mkv")
        );
        assert_eq!(
            partial_sibling(Path::new("/path/to/foo.mp4")),
            PathBuf::from("/path/to/foo.partial.mp4")
        );
    }

    #[test]
    fn partial_sibling_handles_extensionless_paths() {
        assert_eq!(
            partial_sibling(Path::new("/tmp/foo")),
            PathBuf::from("/tmp/foo.partial")
        );
    }

    #[test]
    fn partial_sibling_handles_relative_paths() {
        assert_eq!(
            partial_sibling(Path::new("foo.mkv")),
            PathBuf::from("foo.partial.mkv")
        );
        assert_eq!(
            partial_sibling(Path::new("./foo.mkv")),
            PathBuf::from("./foo.partial.mkv")
        );
    }

    #[test]
    fn partial_sibling_preserves_double_extensions() {
        // A user-given filename like `archive.tar.gz` has Path::extension()
        // returning "gz" and Path::file_stem() returning "archive.tar". So
        // the partial becomes `archive.tar.partial.gz`. We don't pretend to
        // be smart about double extensions; this is the correct behavior
        // for ffmpeg's purposes (the LAST component is what its muxer
        // inference looks at).
        assert_eq!(
            partial_sibling(Path::new("/tmp/archive.tar.gz")),
            PathBuf::from("/tmp/archive.tar.partial.gz")
        );
    }

    /// Regression: ffmpeg's concat demuxer resolves relative paths in the
    /// list file relative to the list-file's *directory*, not the caller's
    /// CWD. Since our list file is under /tmp, a bare `foo.mkv` argument
    /// used to be looked up as `/tmp/foo.mkv` and fail with "Impossible to
    /// open". The fix is to absolutize every input before writing.
    #[test]
    fn write_concat_list_absolutizes_relative_paths() {
        let inputs: &[&Path] = &[Path::new("fo1.mkv"), Path::new("./fo2.mkv")];
        let f = write_concat_list(inputs).expect("write list");
        let content = std::fs::read_to_string(f.path()).expect("read list");

        // Each line must start with "file '/" — i.e. the path is absolute.
        for (i, line) in content.lines().enumerate() {
            assert!(
                line.starts_with("file '/"),
                "line {i} should be absolute; got: {line}"
            );
        }
        // And the basenames must still be present at the end.
        assert!(content.contains("fo1.mkv'"), "content:\n{content}");
        assert!(content.contains("fo2.mkv'"), "content:\n{content}");
    }
}
