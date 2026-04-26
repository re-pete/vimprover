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

#[test]
fn remuxes_mpegps_to_mkv_and_appends_extension() {
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
        stdout.contains("Remux to Matroska"),
        "expected plan to mention Matroska remux:\n{stdout}"
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
        "remuxed output is suspiciously small: {size} bytes"
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
