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
