//! Human-readable rendering of pipeline artifacts.
//!
//! For step 1 this only renders a [`MediaProfile`]. Recipe rendering (for the
//! interactive Y/N prompt) will live here once [`crate::plan`] is implemented.

use std::fmt::Write as _;
use std::path::Path;

use crate::model::{AudioInfo, MediaProfile, Rational, VideoInfo};

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
}
