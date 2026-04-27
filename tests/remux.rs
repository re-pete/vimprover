//! End-to-end test: generate a synthetic legacy-container file with the system
//! ffmpeg, then invoke the `vimprover` binary against it and verify the result.
//!
//! The test gracefully skips (passes with a printed note) if ffmpeg is not
//! available, so this still works on systems without it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn vimprover_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_vimprover"))
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Generate a small DivX-style AVI with B-frames. This is the minimum
/// configuration that reproduces the "Timestamps are unset" MKV-muxer error,
/// because the AVI demuxer hands MPEG-4 ASP packets to the muxer without
/// per-packet PTS and reordered frames can't be inferred.
fn synthesize_divx_avi(out: &Path) -> bool {
    Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=320x240:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:v",
            "mpeg4",
            "-vtag",
            "XVID",
            "-bf",
            "2",
            "-g",
            "12",
            "-b:v",
            "400k",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "128k",
            "-f",
            "avi",
        ])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Generate a "big" 1080p H.264/AAC MP4 — content used by the step-5
/// shrink-mode integration test. The source itself is fine; the test
/// exercises shrink's downscale + ABR encoding regardless of source size.
fn synthesize_1080p_mp4(out: &Path) -> bool {
    Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=1920x1080:rate=24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-ac",
            "2",
        ])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Generate a "fine" file: progressive H.264 + AAC stereo in MP4, square
/// pixels, yuv420p. assess() should report zero issues. Used to verify the
/// step-4 "already fine, refuse without --force" gate.
fn synthesize_fine_mp4(out: &Path) -> bool {
    Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=320x240:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-ac",
            "2",
            "-movflags",
            "+faststart",
        ])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Generate a tiny interlaced MPEG-2 program-stream with non-square pixels —
/// the canonical "DVD-shaped" input that exercises both deinterlace and
/// square-pixel correction in the re-encode planner.
fn synthesize_interlaced_mpeg2_ps(out: &Path) -> bool {
    Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=720x480:rate=30000/1001",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:v",
            "mpeg2video",
            "-flags",
            "+ildct+ilme",
            "-top",
            "1",
            "-aspect",
            "4:3",
            "-b:v",
            "800k",
            "-c:a",
            "ac3",
            "-b:a",
            "192k",
            "-f",
            "vob",
        ])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Generate a 352×240 MPEG-1 / MP2 / MPEG-PS source with SAR 200:219 — the
/// classic NTSC VCD/MPG capture shape. SAR 200:219 yields display width
/// 352 × 200/219 ≈ 321.46 → 321, an *odd* number that libx264/x265 in 4:2:0
/// reject ("width not divisible by 2"). The planner must round the
/// SAR-correction Scale dims down to even before handing them to the
/// encoder; this synthesizer is the regression case for that fix.
fn synthesize_odd_display_width_mpeg1(out: &Path) -> bool {
    Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=352x240:rate=30000/1001",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:v",
            "mpeg1video",
            "-vf",
            "setsar=200/219",
            "-b:v",
            "1100k",
            "-c:a",
            "mp2",
            "-b:a",
            "128k",
            "-f",
            "mpeg",
        ])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Generate a small MPEG program-stream file that looks like a DVD VOB:
/// MPEG-2 video + AC-3 audio in an MPEG-PS container.
fn synthesize_mpeg_ps(out: &Path) -> bool {
    Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=320x240:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:v",
            "mpeg2video",
            "-b:v",
            "500k",
            "-c:a",
            "ac3",
            "-b:a",
            "192k",
            "-f",
            "vob",
        ])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Auto intent on a legacy MPEG-PS / MPEG-2 source. Step-4 assess() correctly
/// flags `LegacyVideoCodec(MPEG-2)`, which causes Auto to route to
/// `plan_reencode` rather than `plan_remux` — so the assertion is now
/// "issues block surfaces, plan re-encodes to H.264 MKV, output exists."
#[test]
fn auto_intent_reencodes_legacy_mpegps_to_h264_mkv() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("input.vob");
    let output_stem = dir.path().join("newname"); // no extension
    let expected_output = dir.path().join("newname.mkv");

    assert!(
        synthesize_mpeg_ps(&input),
        "ffmpeg failed to synthesize a test MPEG-PS file"
    );
    assert!(input.exists(), "synthesized input does not exist");

    let output = Command::new(vimprover_bin())
        .arg("--yes")
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        output.status.success(),
        "vimprover failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Container: MPEG-PS"),
        "expected probe to identify MPEG-PS in stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("Issues:") && stdout.contains("legacy video codec (MPEG-2)"),
        "expected assessment to flag legacy MPEG-2 codec:\n{stdout}"
    );
    assert!(
        stdout.contains("Re-encode video to H.264"),
        "expected Auto to route to H.264 re-encode for legacy MPEG-2:\n{stdout}"
    );

    assert!(
        expected_output.exists(),
        "expected output file {} to exist",
        expected_output.display()
    );

    let size = std::fs::metadata(&expected_output)
        .expect("stat output")
        .len();
    assert!(
        size > 1_000,
        "output is suspiciously small: {size} bytes"
    );
}

#[test]
fn dry_run_does_not_create_output() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("input.vob");
    let output_stem = dir.path().join("newname");
    let expected_output = dir.path().join("newname.mkv");

    assert!(synthesize_mpeg_ps(&input));

    let output = Command::new(vimprover_bin())
        .arg("--dry-run")
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(output.status.success(), "vimprover --dry-run failed");
    assert!(
        !expected_output.exists(),
        "--dry-run should not have created {}",
        expected_output.display()
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("dry run"), "missing dry-run notice:\n{stdout}");
    assert!(
        stdout.contains("Command:"),
        "expected printed command line:\n{stdout}"
    );
}

#[test]
fn refuses_to_overwrite_existing_output_without_flag() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("input.vob");
    let output = dir.path().join("existing.mkv");

    assert!(synthesize_mpeg_ps(&input));
    std::fs::write(&output, b"existing contents").expect("seed existing output");

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg(&input)
        .arg(&output)
        .output()
        .expect("spawn vimprover");

    assert!(
        !result.status.success(),
        "vimprover should have refused to overwrite"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("already exists"),
        "expected 'already exists' in stderr:\n{stderr}"
    );

    // Sanity: existing file is untouched.
    let body = std::fs::read(&output).expect("read existing");
    assert_eq!(body, b"existing contents");
}

/// `--reencode` end-to-end on an interlaced MPEG-2 / MPEG-PS source. Verifies
/// that the planner picks H.264 + bwdif and produces a progressive H.264 MKV.
#[test]
fn reencodes_interlaced_mpeg2_to_progressive_h264() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("input.vob");
    let output_stem = dir.path().join("out");
    let expected_output = dir.path().join("out.mkv");

    assert!(
        synthesize_interlaced_mpeg2_ps(&input),
        "synthesize interlaced MPEG-PS source"
    );

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg("--reencode")
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover --reencode failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("Re-encode video to H.264"),
        "expected H.264 re-encode plan:\n{stdout}"
    );
    assert!(
        stdout.contains("Deinterlace with bwdif"),
        "expected deinterlace step in plan:\n{stdout}"
    );
    assert!(
        stdout.contains("Set square pixel aspect ratio"),
        "expected square-pixel correction step in plan:\n{stdout}"
    );
    assert!(expected_output.exists(), "no output file");

    // ffprobe the output and verify codec + field order.
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,field_order",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(&expected_output)
        .output()
        .expect("spawn ffprobe");
    let probe_str = String::from_utf8_lossy(&probe.stdout);
    assert!(
        probe_str.contains("codec_name=h264"),
        "expected H.264 in output:\n{probe_str}"
    );
    assert!(
        probe_str.contains("field_order=progressive"),
        "expected progressive output:\n{probe_str}"
    );
}

/// Regression: a 352×240 SAR 200:219 MPEG-1 source (NTSC VCD/MPG-capture
/// shape) yields display width 321 (odd), which libx264/x265 in 4:2:0 reject
/// with "width not divisible by 2". The planner rounds the SAR-correction
/// Scale dims down to even (320×240); this end-to-end test verifies the fix
/// holds through ffmpeg actually producing a valid output.
#[test]
fn upgrade_handles_odd_display_width_from_sar_correction() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("clip.mpg");
    let expected_output = dir.path().join("clip.mkv");

    assert!(
        synthesize_odd_display_width_mpeg1(&input),
        "synthesize 352x240 SAR 200:219 MPEG-1 source"
    );

    let result = Command::new(vimprover_bin())
        .arg("--upgrade")
        .arg("--yes")
        .arg(&input)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover --upgrade failed (the bug is back?): \
         status={:?}\nstdout:\n{}\nstderr:\n{}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("Scale to 320x240"),
        "plan must round odd display width 321 down to 320:\n{stdout}"
    );
    assert!(
        stdout.contains("Set square pixel aspect ratio"),
        "expected SAR-correction step in plan:\n{stdout}"
    );

    // Probe the encoded output and confirm both dimensions are even.
    assert!(expected_output.exists(), "expected output at {}",
            expected_output.display());
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height,sample_aspect_ratio",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(&expected_output)
        .output()
        .expect("spawn ffprobe");
    let probe_str = String::from_utf8_lossy(&probe.stdout);
    assert!(
        probe_str.contains("codec_name=h264"),
        "expected H.264 output:\n{probe_str}"
    );
    assert!(
        probe_str.contains("width=320"),
        "expected width 320 (even):\n{probe_str}"
    );
    assert!(
        probe_str.contains("height=240"),
        "expected height 240 (even):\n{probe_str}"
    );
}

/// Forcing x265 + MP4 + custom CRF should round-trip cleanly to a valid file.
#[test]
fn reencode_x265_to_mp4_works() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("input.vob");
    let output_stem = dir.path().join("out");
    let expected_output = dir.path().join("out.mp4");

    assert!(synthesize_mpeg_ps(&input));

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg("--reencode")
        .arg("--video-codec")
        .arg("x265")
        .arg("--container")
        .arg("mp4")
        .arg("--crf")
        .arg("30") // crank speed up for the test
        .arg("--preset")
        .arg("ultrafast")
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover failed: status={:?}\nstderr:\n{}",
        result.status,
        String::from_utf8_lossy(&result.stderr),
    );
    assert!(expected_output.exists());

    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name:format=format_name",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(&expected_output)
        .output()
        .expect("spawn ffprobe");
    let probe_str = String::from_utf8_lossy(&probe.stdout);
    assert!(
        probe_str.contains("codec_name=hevc"),
        "expected HEVC video codec:\n{probe_str}"
    );
    // ISO-BMFF MP4 reports as `mov,mp4,m4a,3gp,3g2,mj2`.
    assert!(
        probe_str.contains("mp4"),
        "expected MP4 container:\n{probe_str}"
    );
}

/// Regression: AVI carrying MPEG-4 ASP (Xvid/DivX) with B-frames hits
/// ``[matroska] Timestamps are unset in a packet`` at mux time unless we pass
/// `-fflags +genpts` on the input side.
#[test]
fn remuxes_divx_avi_with_bframes_to_mkv() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("input.avi");
    let output_stem = dir.path().join("out"); // planner picks .mkv
    let expected_output = dir.path().join("out.mkv");

    assert!(
        synthesize_divx_avi(&input),
        "failed to synthesize DivX-in-AVI test input"
    );

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover failed on DivX-in-AVI: status={:?}\nstdout:\n{}\nstderr:\n{}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("MPEG-4 Part 2"),
        "expected probe to identify MPEG-4 Part 2:\n{stdout}"
    );
    assert!(
        expected_output.exists(),
        "expected output file {} to exist",
        expected_output.display()
    );
}

/// Step-4 fine-gate: a file that passes every assess() rule should be
/// refused without `--force`. With `--force`, the same file processes
/// (Auto + no issues → plan_remux → MKV stream-copy in this case).
#[test]
fn fine_file_is_rejected_without_force_and_processes_with_force() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("input.mp4");
    let output_stem = dir.path().join("out");
    // No extension on the output ⇒ planner picks MKV (its default).
    let expected_output = dir.path().join("out.mkv");

    assert!(
        synthesize_fine_mp4(&input),
        "failed to synthesize fine MP4 input"
    );

    // Without --force, vimprover should refuse: print the profile, no Issues
    // block, and bail with a non-zero exit code.
    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        !result.status.success(),
        "vimprover should have refused a fine file without --force: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !stdout.contains("Issues:"),
        "fine file should not produce an Issues block:\n{stdout}"
    );
    assert!(
        stderr.contains("already fine"),
        "expected 'already fine' rejection in stderr:\n{stderr}"
    );
    assert!(
        !expected_output.exists(),
        "no output should be written when refusing a fine file"
    );

    // With --force, vimprover proceeds (Auto + no issues → remux to the
    // user's chosen container, which is .mp4 here).
    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg("--force")
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover --force on fine file failed: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        expected_output.exists(),
        "expected --force output file at {}",
        expected_output.display()
    );
}

/// Step-5: `--intent shrink --max-height 720` on a 1080p source should
/// downscale to 720p, CRF-encode with x265 (shrink defaults), and produce
/// an MKV. Bypasses the fine-gate (explicit intent).
#[test]
fn shrink_with_max_height_downscales_1080p_to_720p() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("input.mp4");
    let output_stem = dir.path().join("out");
    let expected_output = dir.path().join("out.mkv");

    assert!(
        synthesize_1080p_mp4(&input),
        "failed to synthesize 1080p test input"
    );

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg("--intent")
        .arg("shrink")
        .arg("--max-height")
        .arg("720")
        .arg("--preset")
        .arg("ultrafast") // crank speed up for the test
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover --intent shrink failed: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("Re-encode video to H.265"),
        "plan should use x265 (shrink default): \n{stdout}"
    );
    assert!(
        stdout.contains("CRF 24"),
        "plan should use the x265 ≤720p CRF default (24): \n{stdout}"
    );
    assert!(
        stdout.contains("Scale to 1280x720"),
        "plan should include downscale: \n{stdout}"
    );
    assert!(
        expected_output.exists(),
        "expected output at {}",
        expected_output.display()
    );

    // Probe the output and confirm it's actually 720p HEVC in MKV.
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,height,width:format=format_name",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(&expected_output)
        .output()
        .expect("spawn ffprobe");
    let probe_str = String::from_utf8_lossy(&probe.stdout);
    assert!(
        probe_str.contains("codec_name=hevc"),
        "expected HEVC/H.265 video codec in output:\n{probe_str}"
    );
    assert!(
        probe_str.contains("height=720"),
        "expected 720p output:\n{probe_str}"
    );
    assert!(
        probe_str.contains("width=1280"),
        "expected 1280-wide output (16:9 from 1920x1080):\n{probe_str}"
    );
    assert!(
        probe_str.contains("matroska"),
        "expected Matroska/MKV container:\n{probe_str}"
    );
}

/// Step-5: shrink mode refuses to act when there's literally nothing to
/// shrink — the source is already at-or-below the per-resolution threshold
/// and the user gave no explicit knob. The error must be informative.
#[test]
fn shrink_refuses_already_small_source() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("input.mp4");
    let output_stem = dir.path().join("out");

    // synthesize_1080p_mp4 produces a low-bitrate file (testsrc compresses
    // very well at ultrafast), well below the 1080p threshold.
    assert!(synthesize_1080p_mp4(&input));

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg("--intent")
        .arg("shrink")
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        !result.status.success(),
        "vimprover should have refused: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("source is already")
            && stderr.contains("--target-bitrate"),
        "expected NothingToShrink error in stderr:\n{stderr}"
    );

    // No output written.
    assert!(
        !dir.path().join("out.mkv").exists(),
        "no output should be written when refusing"
    );
}

/// Synthesize a small uniform 480p H.264/AAC MP4 with the given audio
/// frequency, used to make multiple distinguishable-but-uniform inputs for
/// concat tests.
fn synthesize_uniform_480p_mp4(out: &Path, audio_freq_hz: u32) -> bool {
    Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=640x480:rate=24",
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency={audio_freq_hz}:duration=2"),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-ac",
            "2",
        ])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Step-6 happy path: two uniform 480p MP4 clips concat into a single MKV
/// via stream-copy. Verifies the duration is approximately the sum of the
/// inputs.
#[test]
fn concat_two_uniform_clips_produces_joined_mkv() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let clip1 = dir.path().join("clip1.mp4");
    let clip2 = dir.path().join("clip2.mp4");
    let output_stem = dir.path().join("joined");
    let expected_output = dir.path().join("joined.mkv");

    assert!(synthesize_uniform_480p_mp4(&clip1, 440), "synthesize clip1");
    assert!(synthesize_uniform_480p_mp4(&clip2, 880), "synthesize clip2");

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg(&clip1)
        .arg(&clip2)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover concat failed: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("Concat 2 inputs via demuxer (stream copy)"),
        "plan should mention demuxer concat: \n{stdout}"
    );
    assert!(
        expected_output.exists(),
        "expected output at {}",
        expected_output.display()
    );

    // Duration: sum of the two 2s inputs. Allow ±0.2s of slack for boundary
    // adjustments by the demuxer.
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&expected_output)
        .output()
        .expect("spawn ffprobe");
    let duration: f64 = String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse()
        .expect("parse duration");
    assert!(
        (3.8..=4.2).contains(&duration),
        "expected ~4.0s output (2 + 2), got {duration}s"
    );
}

/// Step-6 explicit-intent variant: same uniform-clips scenario, but with
/// `--intent concat` passed explicitly. Should produce the same result.
#[test]
fn concat_with_explicit_intent_works() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let clip1 = dir.path().join("a.mp4");
    let clip2 = dir.path().join("b.mp4");
    let output_stem = dir.path().join("joined");
    let expected_output = dir.path().join("joined.mkv");

    assert!(synthesize_uniform_480p_mp4(&clip1, 440));
    assert!(synthesize_uniform_480p_mp4(&clip2, 880));

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg("--intent")
        .arg("concat")
        .arg(&clip1)
        .arg(&clip2)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover --intent concat failed: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(expected_output.exists());
}

/// Step-6 refusal: clips with different resolutions can't demuxer-concat,
/// and the error message must be precise about *which* input differs and
/// *what* differs.
#[test]
fn concat_refuses_mismatched_resolution_with_actionable_error() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let clip480 = dir.path().join("clip480.mp4");
    let clip720 = dir.path().join("clip720.mp4");
    let output_stem = dir.path().join("joined");

    assert!(synthesize_uniform_480p_mp4(&clip480, 440), "synthesize 480p");
    // Reuse synthesize_1080p_mp4? Different resolution would be cleaner.
    // Just inline a 720p synth here.
    let ok = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=1280x720:rate=24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-ac",
            "2",
        ])
        .arg(&clip720)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "synthesize 720p");

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg(&clip480)
        .arg(&clip720)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        !result.status.success(),
        "vimprover should have refused: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("concat refused"),
        "expected ConcatInputsDiffer in stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("clip720.mp4"),
        "stderr should name the offending input:\n{stderr}"
    );
    assert!(
        stderr.contains("resolution"),
        "stderr should explain the mismatch:\n{stderr}"
    );
    assert!(
        stderr.contains("normalize"),
        "stderr should hint at the fix:\n{stderr}"
    );

    // No output written.
    assert!(
        !dir.path().join("joined.mkv").exists(),
        "no output should be written when refusing concat"
    );
}

/// Step-7 atomic-output: after a successful run the partial file is renamed
/// away, so only the final output exists at the user-visible path. No
/// `.partial.mkv` litter behind.
#[test]
fn successful_run_leaves_only_the_final_output() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("source.mp4");
    let output_stem = dir.path().join("renamed");
    let final_output = dir.path().join("renamed.mkv");
    let partial_output = dir.path().join("renamed.partial.mkv");

    assert!(
        synthesize_uniform_480p_mp4(&input, 440),
        "synthesize source failed"
    );

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg("--force") // synthesized 480p MP4 is "fine"; force to exercise the encode path
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover failed: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(final_output.exists(), "final output should exist");
    assert!(
        !partial_output.exists(),
        "partial sibling should NOT exist after success: {}",
        partial_output.display()
    );
}

/// Step-7 atomic-output: a stale `.partial.mkv` from a prior failed run
/// must be refused (rather than silently clobbered) when `--overwrite`
/// isn't passed. The error names the partial path so the user knows what
/// to remove.
#[test]
fn refuses_stale_partial_without_overwrite() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("source.mp4");
    let output_stem = dir.path().join("target");
    let stale_partial = dir.path().join("target.partial.mkv");

    assert!(synthesize_uniform_480p_mp4(&input, 440));

    // Pre-seed a stale partial. Bytes don't matter — vimprover refuses on
    // existence alone.
    std::fs::write(&stale_partial, b"stale partial garbage").expect("seed partial");

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg("--force")
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        !result.status.success(),
        "vimprover should refuse: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("partial output from a prior run exists"),
        "expected partial-exists message:\n{stderr}"
    );
    assert!(
        stderr.contains("target.partial.mkv"),
        "stderr should name the partial:\n{stderr}"
    );
    assert!(
        stderr.contains("--overwrite"),
        "stderr should hint at --overwrite:\n{stderr}"
    );

    // The stale partial should still be there, untouched.
    assert!(stale_partial.exists());
    let content = std::fs::read(&stale_partial).expect("read partial");
    assert_eq!(content, b"stale partial garbage", "partial must be untouched");

    // No final output written.
    assert!(!dir.path().join("target.mkv").exists());
}

/// Step-7 atomic-output: `--overwrite` removes a stale partial and
/// proceeds to a successful encode.
#[test]
fn overwrite_clears_stale_partial_and_succeeds() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("source.mp4");
    let output_stem = dir.path().join("out");
    let stale_partial = dir.path().join("out.partial.mkv");
    let final_output = dir.path().join("out.mkv");

    assert!(synthesize_uniform_480p_mp4(&input, 440));
    std::fs::write(&stale_partial, b"stale").expect("seed partial");

    let result = Command::new(vimprover_bin())
        .arg("--yes")
        .arg("--force")
        .arg("--overwrite")
        .arg(&input)
        .arg(&output_stem)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover failed: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(final_output.exists());
    assert!(
        !stale_partial.exists(),
        "stale partial should have been replaced"
    );
}

// -----------------------------------------------------------------------
// --upgrade flow (single input, no positional output)
// -----------------------------------------------------------------------

/// Case A: legacy container → modern container. Output gets the new
/// extension and the original is preserved unchanged at its original path.
#[test]
fn upgrade_legacy_avi_writes_mkv_and_preserves_original() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("clip.avi");
    let expected_output = dir.path().join("clip.mkv");

    assert!(synthesize_divx_avi(&input), "synthesize divx avi");
    let original_size = std::fs::metadata(&input).expect("stat input").len();

    let result = Command::new(vimprover_bin())
        .arg("--upgrade")
        .arg("--yes")
        .arg(&input)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover failed: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("will be preserved unchanged"),
        "case-A plan should announce no rename:\n{stdout}"
    );

    // Original survives at its original path with its original size.
    assert!(input.exists(), "original was deleted unexpectedly");
    assert_eq!(
        std::fs::metadata(&input).expect("stat input").len(),
        original_size,
        "original file was modified"
    );
    // New output exists.
    assert!(expected_output.exists(), "expected output not created");
    // No backup was produced.
    assert!(
        !dir.path().join("clip.vimprover-orig.avi").exists(),
        "no backup should be produced when extensions differ"
    );
}

/// Case B: same container → reencode in place. Output collides with input,
/// so the original is renamed aside to `<stem>.vimprover-orig.<ext>` before
/// encoding.
#[test]
fn upgrade_same_container_creates_backup() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("clip.mp4");
    let expected_backup = dir.path().join("clip.vimprover-orig.mp4");

    assert!(synthesize_uniform_480p_mp4(&input, 440), "synthesize mp4");
    let original_size = std::fs::metadata(&input).expect("stat input").len();

    let result = Command::new(vimprover_bin())
        .arg("--upgrade")
        .arg("--reencode")
        .arg("--container")
        .arg("mp4")
        .arg("--yes")
        .arg(&input)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover failed: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("will be renamed to"),
        "case-B plan should announce backup rename:\n{stdout}"
    );
    assert!(
        stdout.contains("Original preserved at:"),
        "case-B summary should print backup path:\n{stdout}"
    );

    // Backup exists with the original's size.
    assert!(expected_backup.exists(), "backup not created");
    assert_eq!(
        std::fs::metadata(&expected_backup).expect("stat backup").len(),
        original_size,
        "backup is not the original file"
    );
    // New file at the original path.
    assert!(input.exists(), "encoded file should be at original path");
}

/// Case B with a pre-existing backup file: refuse rather than clobber it.
#[test]
fn upgrade_refuses_stale_backup_without_overwrite() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("clip.mp4");
    let stale_backup = dir.path().join("clip.vimprover-orig.mp4");

    assert!(synthesize_uniform_480p_mp4(&input, 440));
    std::fs::write(&stale_backup, b"stale-backup-from-prior-run")
        .expect("seed stale backup");

    let result = Command::new(vimprover_bin())
        .arg("--upgrade")
        .arg("--reencode")
        .arg("--container")
        .arg("mp4")
        .arg("--yes")
        .arg(&input)
        .output()
        .expect("spawn vimprover");

    assert!(
        !result.status.success(),
        "expected vimprover to refuse, but it succeeded"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("backup file from a prior upgrade exists"),
        "expected stale-backup refusal message:\n{stderr}"
    );

    // Stale backup is preserved (not clobbered).
    let stale_contents =
        std::fs::read(&stale_backup).expect("read stale backup after refusal");
    assert_eq!(stale_contents, b"stale-backup-from-prior-run");
}

/// Case B with `--overwrite`: stale backup is cleared and the upgrade
/// succeeds, with the new backup containing the *current* original.
#[test]
fn upgrade_overwrite_replaces_stale_backup() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("clip.mp4");
    let backup_path = dir.path().join("clip.vimprover-orig.mp4");

    assert!(synthesize_uniform_480p_mp4(&input, 440));
    let original_size = std::fs::metadata(&input).expect("stat input").len();
    std::fs::write(&backup_path, b"stale-backup-from-prior-run")
        .expect("seed stale backup");

    let result = Command::new(vimprover_bin())
        .arg("--upgrade")
        .arg("--reencode")
        .arg("--container")
        .arg("mp4")
        .arg("--overwrite")
        .arg("--yes")
        .arg(&input)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "vimprover failed: stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    // Backup now holds the *current* original (not the stale-backup bytes).
    let backup_size = std::fs::metadata(&backup_path).expect("stat backup").len();
    assert_eq!(
        backup_size, original_size,
        "backup should be the freshly-renamed original, not the stale bytes"
    );
}

/// `--upgrade` rejects multi-input invocations: it's a single-file mode.
#[test]
fn upgrade_with_multiple_inputs_is_rejected() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let a = dir.path().join("a.mp4");
    let b = dir.path().join("b.mp4");
    assert!(synthesize_uniform_480p_mp4(&a, 440));
    assert!(synthesize_uniform_480p_mp4(&b, 880));

    let result = Command::new(vimprover_bin())
        .arg("--upgrade")
        .arg("--yes")
        .arg(&a)
        .arg(&b)
        .output()
        .expect("spawn vimprover");

    assert!(
        !result.status.success(),
        "vimprover should refuse --upgrade with 2 inputs"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("--upgrade takes exactly one INPUT"),
        "expected explicit 1-input refusal:\n{stderr}"
    );

    // Neither input was modified, no output produced.
    assert!(a.exists() && b.exists());
    assert!(!dir.path().join("a.mkv").exists());
    assert!(!dir.path().join("a.vimprover-orig.mp4").exists());
}

/// `--upgrade --dry-run` prints the plan and the ffmpeg command without
/// touching the filesystem at all (no rename, no output, no backup).
#[test]
fn upgrade_dry_run_does_not_touch_files() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found on PATH; skipping integration test");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let input = dir.path().join("clip.mp4");
    assert!(synthesize_uniform_480p_mp4(&input, 440));
    let original_size = std::fs::metadata(&input).expect("stat input").len();

    let result = Command::new(vimprover_bin())
        .arg("--upgrade")
        .arg("--reencode")
        .arg("--container")
        .arg("mp4")
        .arg("--dry-run")
        .arg(&input)
        .output()
        .expect("spawn vimprover");

    assert!(
        result.status.success(),
        "dry-run upgrade should succeed: stderr:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("Plan:"), "expected plan in stdout:\n{stdout}");
    assert!(
        stdout.contains("(dry run — not executing)"),
        "expected dry-run notice:\n{stdout}"
    );

    // Filesystem is untouched.
    assert!(input.exists(), "input should still exist");
    assert_eq!(
        std::fs::metadata(&input).expect("stat input").len(),
        original_size,
        "input was modified by dry-run"
    );
    assert!(
        !dir.path().join("clip.vimprover-orig.mp4").exists(),
        "dry-run should not create a backup"
    );
}
