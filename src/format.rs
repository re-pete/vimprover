//! Human-readable rendering of pipeline artifacts.
//!
//! Step 1 added [`render_profile`]. Step 2 adds [`render_recipe`] for the
//! plan-summary block printed before ffmpeg runs.

use std::fmt::Write as _;
use std::path::Path;

use crate::assess::{Assessment, Issue};
use crate::model::{AudioInfo, MediaProfile, Rational, VideoInfo};
use crate::plan::{AudioStrategy, ConcatStrategy, EncodeRecipe, VideoFilter, VideoStrategy};

/// Produce the human-readable "Input:" block for a single file.
pub fn render_profile(path: &Path, profile: &MediaProfile) -> String {
    let mut out = String::with_capacity(256);

    let _ = writeln!(out, "File:      {}", path.display());

    // Container line
    let mut container_line = format!("Container: {}", profile.container);
    if let Some(size) = profile.file_size_bytes {
        let _ = write!(container_line, ", {}", format_bytes(size));
    }
    if let Some(dur) = profile.duration_secs {
        let _ = write!(container_line, ", {}", format_duration(dur));
    }
    if let Some(br) = profile.bitrate_bps {
        let _ = write!(container_line, ", {}", format_bitrate(br));
    }
    let _ = writeln!(out, "{container_line}");

    // Video line
    let _ = writeln!(out, "Video:     {}", render_video(&profile.video));

    // Audio line(s)
    if profile.audio.is_empty() {
        let _ = writeln!(out, "Audio:     (none)");
    } else {
        for (i, a) in profile.audio.iter().enumerate() {
            let _ = writeln!(out, "Audio {}:   {}", i + 1, render_audio(a));
        }
    }

    // Trim trailing newline for consistent `println!` behavior at call sites.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Render a plan-summary block describing what `recipe` will do.
///
/// Output uses the same column alignment as [`render_profile`]: the first line
/// starts with `Plan:` and subsequent lines indent under the value column.
pub fn render_recipe(recipe: &EncodeRecipe, output: &Path) -> String {
    let lines = collect_plan_lines(recipe, output);
    render_aligned_block("Plan:", &lines)
}

/// Render a plan-summary block for a multi-input concat operation.
///
/// `inputs` is the ordered list of source files; `recipe` must have
/// `concat == Some(_)` (other callers should use [`render_recipe`] instead).
pub fn render_concat_recipe(
    recipe: &EncodeRecipe,
    inputs: &[&Path],
    output: &Path,
) -> String {
    let lines = collect_concat_plan_lines(recipe, inputs, output);
    render_aligned_block("Plan:", &lines)
}

/// Render an assessment as a single "Issues:" block, or `None` if the file
/// is fine. Matches the column alignment of [`render_profile`] and
/// [`render_recipe`].
pub fn render_assessment(assessment: &Assessment) -> Option<String> {
    if assessment.is_fine() {
        return None;
    }
    let lines: Vec<String> = assessment.issues.iter().map(describe_issue).collect();
    Some(render_aligned_block("Issues:", &lines))
}

/// Shared block-renderer for the "Field:    value" multi-line shape.
fn render_aligned_block(label: &str, lines: &[String]) -> String {
    // Pad the label to the same column width as render_profile uses
    // ("File:      " = 11 chars, label + spaces).
    const WIDTH: usize = 11;
    let pad_label = format!("{label}{:width$}", "", width = WIDTH.saturating_sub(label.len()));
    let indent = " ".repeat(WIDTH);

    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            let _ = write!(out, "{pad_label}{line}");
        } else {
            let _ = write!(out, "\n{indent}{line}");
        }
    }
    out
}

fn describe_issue(issue: &Issue) -> String {
    match issue {
        Issue::LegacyVideoCodec(c) => format!("legacy video codec ({c})"),
        Issue::LegacyAudioCodec(c) => format!("legacy audio codec ({c})"),
        Issue::LegacyContainer(c) => format!("legacy container ({c})"),
        Issue::MissingSeekIndex => "missing seek index".into(),
        Issue::Interlaced => "interlaced source".into(),
        Issue::NonModernPixelFormat(pf) => format!("non-modern pixel format ({pf})"),
        Issue::BitrateExcessive { actual, threshold } => format!(
            "bitrate {} exceeds threshold {}",
            format_bitrate(*actual),
            format_bitrate(*threshold)
        ),
        Issue::ResolutionWasteful { height, bits_per_pixel_per_sec } => format!(
            "resolution wasteful ({height}p at {bits_per_pixel_per_sec:.2} bits/pixel/sec — \
             downscale won't lose meaningful detail)"
        ),
        Issue::NonSquarePixels { sar_num, sar_den } => {
            format!("non-square pixels (SAR {sar_num}:{sar_den})")
        }
        Issue::AudioCodecIncompatibleWithTargetContainer => {
            "audio codec not supported by target container".into()
        }
    }
}

fn collect_concat_plan_lines(
    recipe: &EncodeRecipe,
    inputs: &[&Path],
    output: &Path,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let strategy = match recipe.concat {
        Some(ConcatStrategy::Demuxer) => "demuxer (stream copy)",
        None => "plain",
    };
    lines.push(format!(
        "Concat {} inputs via {strategy} into {}",
        inputs.len(),
        recipe.output_container
    ));
    for (i, path) in inputs.iter().enumerate() {
        lines.push(format!("  [{}] {}", i + 1, path.display()));
    }
    if matches!(recipe.output_container, crate::model::Container::Mp4)
        && recipe
            .extra_flags
            .iter()
            .any(|f| f == "-movflags" || f == "+faststart")
    {
        lines.push("Enable MP4 faststart (moov before mdat)".into());
    }
    lines.push(format!("Output: {}", output.display()));
    lines
}

fn collect_plan_lines(recipe: &EncodeRecipe, output: &Path) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();

    let pure_copy = matches!(recipe.video_strategy, VideoStrategy::Copy)
        && matches!(recipe.audio_strategy, AudioStrategy::Copy)
        && recipe.video_filters.is_empty();

    if pure_copy {
        lines.push(format!(
            "Remux to {} (stream copy, no re-encode)",
            recipe.output_container
        ));
    } else {
        lines.push(describe_video(recipe));
        for filter in &recipe.video_filters {
            lines.push(describe_filter(filter));
        }
        if let Some(audio_line) = describe_audio(&recipe.audio_strategy) {
            lines.push(audio_line);
        }
        if recipe.extra_flags.windows(2).any(|w| w == ["-color_range", "pc"]) {
            lines.push("Preserve full-range (JPEG) color tagging".into());
        }
        if matches!(recipe.output_container, crate::model::Container::Mp4)
            && recipe
                .extra_flags
                .iter()
                .any(|f| f == "-movflags" || f == "+faststart")
        {
            lines.push("Enable MP4 faststart (moov before mdat)".into());
        }
    }

    lines.push(format!("Output: {}", output.display()));
    lines
}

fn describe_video(recipe: &EncodeRecipe) -> String {
    match &recipe.video_strategy {
        VideoStrategy::Copy => format!(
            "Container → {} (video stream-copy)",
            recipe.output_container
        ),
        VideoStrategy::ReencodeX264 { crf, preset } => format!(
            "Re-encode video to H.264 (CRF {crf}, {preset} preset) in {}",
            recipe.output_container
        ),
        VideoStrategy::ReencodeX265 { crf, preset } => format!(
            "Re-encode video to H.265 (CRF {crf}, {preset} preset) in {}",
            recipe.output_container
        ),
        VideoStrategy::ReencodeX264Abr { target_bps, preset, .. } => format!(
            "Re-encode video to H.264 (ABR {} target, {preset} preset) in {}",
            format_bitrate(*target_bps),
            recipe.output_container
        ),
        VideoStrategy::ReencodeX265Abr { target_bps, preset, .. } => format!(
            "Re-encode video to H.265 (ABR {} target, {preset} preset) in {}",
            format_bitrate(*target_bps),
            recipe.output_container
        ),
    }
}

fn describe_filter(f: &VideoFilter) -> String {
    match f {
        VideoFilter::Bwdif => "Deinterlace with bwdif".into(),
        VideoFilter::FieldmatchDecimate => {
            "Inverse-telecine (fieldmatch + decimate)".into()
        }
        VideoFilter::Scale { width, height } => format!("Scale to {width}x{height}"),
        VideoFilter::SetSar { num: 1, den: 1 } => "Set square pixel aspect ratio".into(),
        VideoFilter::SetSar { num, den } => format!("Set SAR to {num}:{den}"),
    }
}

fn describe_audio(strategy: &AudioStrategy) -> Option<String> {
    match strategy {
        AudioStrategy::Copy => None,
        AudioStrategy::AacStereo { bitrate_bps } => Some(format!(
            "Downmix audio to AAC stereo {} kbps",
            bitrate_bps / 1000
        )),
        AudioStrategy::AacMultichannel { bitrate_bps } => Some(format!(
            "Re-encode audio to AAC multichannel ({} kbps)",
            bitrate_bps / 1000
        )),
    }
}

fn render_video(v: &VideoInfo) -> String {
    let mut line = format!("{}, {}x{}", v.codec, v.width, v.height);

    // Display size differs from coded size when SAR != 1:1.
    let (dw, dh) = v.display_size();
    if (dw, dh) != (v.width, v.height) {
        let _ = write!(line, " (display {dw}x{dh})");
    }

    let _ = write!(line, ", {}", v.field_order);

    if let Some(fr) = v.framerate {
        let _ = write!(line, ", {} fps", render_framerate(fr));
    }
    if let Some(ref pf) = v.pix_fmt {
        let _ = write!(line, ", {pf}");
    }
    if let Some(sar) = v.sar {
        if sar.num != sar.den {
            let _ = write!(line, ", SAR {}:{}", sar.num, sar.den);
        }
    }
    if v.is_hdr {
        line.push_str(", HDR");
    }
    line
}

fn render_audio(a: &AudioInfo) -> String {
    let mut line = format!("{}", a.codec);
    if let Some(layout) = a.channel_layout.as_deref() {
        let _ = write!(line, ", {layout}");
    } else if let Some(ch) = a.channels {
        let _ = write!(line, ", {ch} channel{}", if ch == 1 { "" } else { "s" });
    }
    if let Some(sr) = a.sample_rate_hz {
        let _ = write!(line, ", {} kHz", format_khz(sr));
    }
    if let Some(br) = a.bitrate_bps {
        let _ = write!(line, ", {}", format_bitrate(br));
    }
    if let Some(lang) = a.language.as_deref() {
        let _ = write!(line, " ({lang})");
    }
    line
}

fn render_framerate(r: Rational) -> String {
    let f = r.as_f64();
    if (f - f.round()).abs() < 1e-6 {
        format!("{}", f.round() as u32)
    } else {
        format!("{f:.3}")
    }
}

// ---------------------------------------------------------------------------
// Pretty formatters (bytes, duration, bitrate)
// ---------------------------------------------------------------------------

/// Format a byte count with binary units (KiB/MiB/GiB/TiB), 1 decimal.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Format a duration in seconds as `HhMMmSSs`, omitting leading zero components.
pub fn format_duration(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m{seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// Format an audio sample rate (Hz) in kHz, preserving fractional rates like 44.1.
pub fn format_khz(hz: u32) -> String {
    if hz % 1000 == 0 {
        format!("{}", hz / 1000)
    } else {
        let khz = hz as f64 / 1000.0;
        format!("{khz:.1}")
    }
}

/// Format a bitrate (bits per second) as kbps or Mbps.
pub fn format_bitrate(bps: u64) -> String {
    if bps >= 1_000_000 {
        let mbps = bps as f64 / 1_000_000.0;
        format!("{mbps:.1} Mbps")
    } else if bps >= 1_000 {
        let kbps = bps as f64 / 1_000.0;
        format!("{kbps:.0} kbps")
    } else {
        format!("{bps} bps")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AudioCodec, AudioInfo, Container, FieldOrder, MediaProfile, PixFmt, Rational, VideoCodec,
        VideoInfo,
    };
    use pretty_assertions::assert_eq;

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2_048), "2.0 KiB");
        assert_eq!(format_bytes(1_572_864), "1.5 MiB");
        assert_eq!(format_bytes(4_509_876_543), "4.2 GiB");
    }

    #[test]
    fn formats_duration() {
        assert_eq!(format_duration(0.0), "0s");
        assert_eq!(format_duration(42.0), "42s");
        assert_eq!(format_duration(65.0), "1m05s");
        assert_eq!(format_duration(3_600.0), "1h00m00s");
        assert_eq!(format_duration(5520.36), "1h32m00s");
    }

    #[test]
    fn formats_khz() {
        assert_eq!(format_khz(48_000), "48");
        assert_eq!(format_khz(44_100), "44.1");
        assert_eq!(format_khz(22_050), "22.1"); // rounded to 1dp
        assert_eq!(format_khz(8_000), "8");
    }

    #[test]
    fn formats_bitrate() {
        assert_eq!(format_bitrate(500), "500 bps");
        assert_eq!(format_bitrate(192_000), "192 kbps");
        assert_eq!(format_bitrate(6_534_220), "6.5 Mbps");
    }

    #[test]
    fn renders_full_dvd_profile() {
        let profile = MediaProfile {
            container: Container::MpegPs,
            video: VideoInfo {
                codec: VideoCodec::Mpeg2,
                width: 720,
                height: 480,
                field_order: FieldOrder::InterlacedTopFirst,
                framerate: Rational::new(30000, 1001),
                pix_fmt: Some(PixFmt::Yuv420p),
                sar: Rational::new(10, 11),
                is_hdr: false,
            },
            audio: vec![AudioInfo {
                codec: AudioCodec::Ac3,
                channels: Some(6),
                channel_layout: Some("5.1(side)".into()),
                sample_rate_hz: Some(48_000),
                bitrate_bps: Some(448_000),
                language: Some("eng".into()),
            }],
            duration_secs: Some(5520.36),
            file_size_bytes: Some(4_509_876_543),
            bitrate_bps: Some(6_534_220),
        };
        let out = render_profile(Path::new("movie.vob"), &profile);
        let expected = "File:      movie.vob\n\
                        Container: MPEG-PS, 4.2 GiB, 1h32m00s, 6.5 Mbps\n\
                        Video:     MPEG-2, 720x480 (display 655x480), interlaced (TFF), 29.970 fps, yuv420p, SAR 10:11\n\
                        Audio 1:   AC-3, 5.1(side), 48 kHz, 448 kbps (eng)";
        assert_eq!(out, expected);
    }

    #[test]
    fn renders_recipe_for_pure_remux() {
        let recipe = EncodeRecipe {
            output_container: Container::Mkv,
            video_strategy: VideoStrategy::Copy,
            video_filters: Vec::new(),
            audio_strategy: AudioStrategy::Copy,
            extra_flags: Vec::new(),
            concat: None,
        };
        let out = render_recipe(&recipe, Path::new("newname.mkv"));
        let expected = "Plan:      Remux to Matroska (MKV) (stream copy, no re-encode)\n\
                        \x20          Output: newname.mkv";
        assert_eq!(out, expected);
    }

    #[test]
    fn renders_recipe_with_filters_and_reencode() {
        let recipe = EncodeRecipe {
            output_container: Container::Mp4,
            video_strategy: VideoStrategy::ReencodeX264 {
                crf: 20,
                preset: "medium".into(),
            },
            video_filters: vec![
                VideoFilter::Bwdif,
                VideoFilter::Scale {
                    width: 854,
                    height: 480,
                },
                VideoFilter::SetSar { num: 1, den: 1 },
            ],
            audio_strategy: AudioStrategy::AacStereo { bitrate_bps: 192_000 },
            extra_flags: vec!["-movflags".into(), "+faststart".into()],
            concat: None,
        };
        let out = render_recipe(&recipe, Path::new("/tmp/newname.mp4"));
        assert!(
            out.starts_with("Plan:      Re-encode video to H.264 (CRF 20, medium preset) in MP4"),
            "got:\n{out}"
        );
        assert!(out.contains("Deinterlace with bwdif"));
        assert!(out.contains("Scale to 854x480"));
        assert!(out.contains("Set square pixel aspect ratio"));
        assert!(out.contains("Downmix audio to AAC stereo 192 kbps"));
        assert!(out.contains("Enable MP4 faststart"));
        assert!(out.ends_with("Output: /tmp/newname.mp4"));
    }

    #[test]
    fn renders_assessment_with_multiple_issues() {
        let assessment = Assessment {
            issues: vec![
                Issue::LegacyVideoCodec(VideoCodec::Mpeg2),
                Issue::LegacyContainer(Container::MpegPs),
                Issue::Interlaced,
                Issue::NonSquarePixels { sar_num: 10, sar_den: 11 },
            ],
        };
        let rendered = render_assessment(&assessment).expect("non-fine -> Some");
        let expected = "Issues:    legacy video codec (MPEG-2)\n\
                        \x20          legacy container (MPEG-PS)\n\
                        \x20          interlaced source\n\
                        \x20          non-square pixels (SAR 10:11)";
        assert_eq!(rendered, expected);
    }

    #[test]
    fn renders_assessment_returns_none_when_fine() {
        assert_eq!(render_assessment(&Assessment::default()), None);
    }

    #[test]
    fn renders_profile_without_audio() {
        let profile = MediaProfile {
            container: Container::Mp4,
            video: VideoInfo {
                codec: VideoCodec::H264,
                width: 1920,
                height: 1080,
                field_order: FieldOrder::Progressive,
                framerate: Rational::new(30, 1),
                pix_fmt: Some(PixFmt::Yuv420p),
                sar: Rational::new(1, 1),
                is_hdr: false,
            },
            audio: vec![],
            duration_secs: Some(60.0),
            file_size_bytes: Some(50_000_000),
            bitrate_bps: Some(5_000_000),
        };
        let out = render_profile(Path::new("silent.mp4"), &profile);
        assert!(out.contains("Container: MP4, "));
        assert!(out.contains("Video:     H.264, 1920x1080, progressive, 30 fps, yuv420p"));
        assert!(out.contains("Audio:     (none)"));
    }

    #[test]
    fn renders_concat_recipe_with_input_list() {
        let recipe = EncodeRecipe {
            output_container: Container::Mkv,
            video_strategy: VideoStrategy::Copy,
            video_filters: Vec::new(),
            audio_strategy: AudioStrategy::Copy,
            extra_flags: Vec::new(),
            concat: Some(ConcatStrategy::Demuxer),
        };
        let inputs: Vec<&Path> = vec![
            Path::new("clip1.mp4"),
            Path::new("clip2.mp4"),
            Path::new("clip3.mp4"),
        ];
        let out = render_concat_recipe(&recipe, &inputs, Path::new("joined.mkv"));

        // Header line.
        assert!(
            out.contains("Concat 3 inputs via demuxer (stream copy) into Matroska (MKV)"),
            "got:\n{out}"
        );
        // Numbered list.
        assert!(out.contains("[1] clip1.mp4"), "got:\n{out}");
        assert!(out.contains("[2] clip2.mp4"), "got:\n{out}");
        assert!(out.contains("[3] clip3.mp4"), "got:\n{out}");
        // Output line.
        assert!(out.ends_with("Output: joined.mkv"), "got:\n{out}");
    }

    #[test]
    fn renders_concat_recipe_mp4_output_mentions_faststart() {
        let recipe = EncodeRecipe {
            output_container: Container::Mp4,
            video_strategy: VideoStrategy::Copy,
            video_filters: Vec::new(),
            audio_strategy: AudioStrategy::Copy,
            extra_flags: vec!["-movflags".into(), "+faststart".into()],
            concat: Some(ConcatStrategy::Demuxer),
        };
        let inputs: Vec<&Path> = vec![Path::new("a.mp4"), Path::new("b.mp4")];
        let out = render_concat_recipe(&recipe, &inputs, Path::new("joined.mp4"));
        assert!(out.contains("Concat 2 inputs"), "got:\n{out}");
        assert!(out.contains("Enable MP4 faststart"), "got:\n{out}");
    }
}
