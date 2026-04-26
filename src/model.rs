//! Typed media model shared by every layer of the pipeline.
//!
//! Every enum provides a `from_ffprobe_name` constructor that accepts the raw
//! strings reported by `ffprobe`. Unknown values are preserved in an
//! `Unknown(String)` variant rather than dropped, both for debug output and so
//! the assessor/planner can make conservative decisions in later steps.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Container
// ---------------------------------------------------------------------------

/// High-level container format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Container {
    Mp4,
    Mov,
    Mkv,
    WebM,
    /// MPEG program stream (`.mpg`, `.vob`, etc.).
    MpegPs,
    /// MPEG transport stream (`.ts`, `.m2ts`).
    MpegTs,
    /// ASF / WMV.
    Asf,
    Avi,
    Flv,
    Unknown(String),
}

impl Container {
    /// Best-guess container from ffprobe's comma-separated `format_name`.
    ///
    /// `format_name` looks like `"mov,mp4,m4a,3gp,3g2,mj2"` or `"matroska,webm"`.
    /// The first matched token wins, so Mp4 is preferred over Mov for mp4 files.
    pub fn from_ffprobe_name(format_name: &str) -> Self {
        let parts: Vec<&str> = format_name.split(',').map(str::trim).collect();
        let has = |needle: &str| parts.contains(&needle);
        if has("mp4") {
            Container::Mp4
        } else if has("mov") {
            Container::Mov
        } else if has("matroska") {
            Container::Mkv
        } else if has("webm") {
            Container::WebM
        } else if has("avi") {
            Container::Avi
        } else if has("asf") {
            Container::Asf
        } else if has("flv") {
            Container::Flv
        } else if has("mpeg") {
            Container::MpegPs
        } else if has("mpegts") {
            Container::MpegTs
        } else {
            Container::Unknown(format_name.to_string())
        }
    }

    /// Disambiguate using the filename extension. `ffprobe` reports the same
    /// `format_name` for `.mkv` and `.webm` files, so we refine with the
    /// extension when available.
    pub fn refine_with_filename(self, path: &Path) -> Self {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        match (self, ext.as_deref()) {
            (Container::Mkv, Some("webm")) => Container::WebM,
            (Container::WebM, Some("mkv")) => Container::Mkv,
            (c, _) => c,
        }
    }

    /// Canonical filename extension used when the planner picks this container.
    pub fn canonical_extension(&self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Mov => "mov",
            Container::Mkv => "mkv",
            Container::WebM => "webm",
            Container::MpegPs => "mpg",
            Container::MpegTs => "ts",
            Container::Asf => "wmv",
            Container::Avi => "avi",
            Container::Flv => "flv",
            Container::Unknown(_) => "bin",
        }
    }
}

impl fmt::Display for Container {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Container::Mp4 => f.write_str("MP4"),
            Container::Mov => f.write_str("MOV"),
            Container::Mkv => f.write_str("Matroska (MKV)"),
            Container::WebM => f.write_str("WebM"),
            Container::MpegPs => f.write_str("MPEG-PS"),
            Container::MpegTs => f.write_str("MPEG-TS"),
            Container::Asf => f.write_str("ASF/WMV"),
            Container::Avi => f.write_str("AVI"),
            Container::Flv => f.write_str("FLV"),
            Container::Unknown(s) => write!(f, "unknown ({s})"),
        }
    }
}

// ---------------------------------------------------------------------------
// Video codec
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    Mpeg1,
    Mpeg2,
    /// Generic MPEG-4 Part 2 (Xvid/DivX).
    Mpeg4,
    /// Microsoft MPEG-4 v1/v2/v3.
    Msmpeg4,
    H263,
    H264,
    H265,
    Av1,
    Vp8,
    Vp9,
    Vc1,
    Wmv1,
    Wmv2,
    Wmv3,
    Theora,
    Flv1,
    Unknown(String),
}

impl VideoCodec {
    pub fn from_ffprobe_name(codec_name: &str) -> Self {
        match codec_name {
            "mpeg1video" => VideoCodec::Mpeg1,
            "mpeg2video" => VideoCodec::Mpeg2,
            "mpeg4" => VideoCodec::Mpeg4,
            "msmpeg4v1" | "msmpeg4v2" | "msmpeg4v3" | "msmpeg4" => VideoCodec::Msmpeg4,
            "h263" => VideoCodec::H263,
            "h264" => VideoCodec::H264,
            "hevc" | "h265" => VideoCodec::H265,
            "av1" => VideoCodec::Av1,
            "vp8" => VideoCodec::Vp8,
            "vp9" => VideoCodec::Vp9,
            "vc1" => VideoCodec::Vc1,
            "wmv1" => VideoCodec::Wmv1,
            "wmv2" => VideoCodec::Wmv2,
            "wmv3" => VideoCodec::Wmv3,
            "theora" => VideoCodec::Theora,
            "flv1" => VideoCodec::Flv1,
            other => VideoCodec::Unknown(other.to_string()),
        }
    }
}

impl fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            VideoCodec::Mpeg1 => "MPEG-1",
            VideoCodec::Mpeg2 => "MPEG-2",
            VideoCodec::Mpeg4 => "MPEG-4 Part 2 (Xvid/DivX)",
            VideoCodec::Msmpeg4 => "Microsoft MPEG-4",
            VideoCodec::H263 => "H.263",
            VideoCodec::H264 => "H.264",
            VideoCodec::H265 => "H.265 (HEVC)",
            VideoCodec::Av1 => "AV1",
            VideoCodec::Vp8 => "VP8",
            VideoCodec::Vp9 => "VP9",
            VideoCodec::Vc1 => "VC-1",
            VideoCodec::Wmv1 => "WMV 1",
            VideoCodec::Wmv2 => "WMV 2",
            VideoCodec::Wmv3 => "WMV 3",
            VideoCodec::Theora => "Theora",
            VideoCodec::Flv1 => "Sorenson Spark (FLV1)",
            VideoCodec::Unknown(s) => return write!(f, "unknown video codec ({s})"),
        };
        f.write_str(s)
    }
}

// ---------------------------------------------------------------------------
// Audio codec
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodec {
    Aac,
    Ac3,
    Eac3,
    Dts,
    Mp2,
    Mp3,
    Opus,
    Flac,
    Vorbis,
    Alac,
    /// Covers all `pcm_*` variants; the exact name is preserved.
    Pcm(String),
    /// Covers `wmav1`, `wmav2`, `wmapro`, `wmavoice`, `wmalossless`.
    Wma(String),
    Unknown(String),
}

impl AudioCodec {
    pub fn from_ffprobe_name(codec_name: &str) -> Self {
        match codec_name {
            "aac" => AudioCodec::Aac,
            "ac3" => AudioCodec::Ac3,
            "eac3" => AudioCodec::Eac3,
            "dts" => AudioCodec::Dts,
            "mp2" => AudioCodec::Mp2,
            "mp3" | "mp3float" => AudioCodec::Mp3,
            "opus" => AudioCodec::Opus,
            "flac" => AudioCodec::Flac,
            "vorbis" => AudioCodec::Vorbis,
            "alac" => AudioCodec::Alac,
            other if other.starts_with("pcm_") => AudioCodec::Pcm(other.to_string()),
            other if other.starts_with("wma") => AudioCodec::Wma(other.to_string()),
            other => AudioCodec::Unknown(other.to_string()),
        }
    }
}

impl fmt::Display for AudioCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioCodec::Aac => f.write_str("AAC"),
            AudioCodec::Ac3 => f.write_str("AC-3"),
            AudioCodec::Eac3 => f.write_str("E-AC-3"),
            AudioCodec::Dts => f.write_str("DTS"),
            AudioCodec::Mp2 => f.write_str("MP2"),
            AudioCodec::Mp3 => f.write_str("MP3"),
            AudioCodec::Opus => f.write_str("Opus"),
            AudioCodec::Flac => f.write_str("FLAC"),
            AudioCodec::Vorbis => f.write_str("Vorbis"),
            AudioCodec::Alac => f.write_str("ALAC"),
            AudioCodec::Pcm(s) => write!(f, "PCM ({s})"),
            AudioCodec::Wma(s) => write!(f, "WMA ({s})"),
            AudioCodec::Unknown(s) => write!(f, "unknown audio codec ({s})"),
        }
    }
}

// ---------------------------------------------------------------------------
// Pixel format
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixFmt {
    Yuv420p,
    Yuv420p10le,
    Yuv420p12le,
    Yuv422p,
    Yuv422p10le,
    Yuv444p,
    Yuv444p10le,
    /// Full-range JPEG-style.
    Yuvj420p,
    Yuvj422p,
    Yuvj444p,
    Other(String),
}

impl PixFmt {
    pub fn from_ffprobe_name(name: &str) -> Self {
        match name {
            "yuv420p" => PixFmt::Yuv420p,
            "yuv420p10le" => PixFmt::Yuv420p10le,
            "yuv420p12le" => PixFmt::Yuv420p12le,
            "yuv422p" => PixFmt::Yuv422p,
            "yuv422p10le" => PixFmt::Yuv422p10le,
            "yuv444p" => PixFmt::Yuv444p,
            "yuv444p10le" => PixFmt::Yuv444p10le,
            "yuvj420p" => PixFmt::Yuvj420p,
            "yuvj422p" => PixFmt::Yuvj422p,
            "yuvj444p" => PixFmt::Yuvj444p,
            other => PixFmt::Other(other.to_string()),
        }
    }

    /// Is this a "modern, broadly compatible" pixel format for H.264/H.265/AV1 distribution?
    pub fn is_modern(&self) -> bool {
        matches!(self, PixFmt::Yuv420p | PixFmt::Yuv420p10le)
    }
}

impl fmt::Display for PixFmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PixFmt::Yuv420p => f.write_str("yuv420p"),
            PixFmt::Yuv420p10le => f.write_str("yuv420p10le"),
            PixFmt::Yuv420p12le => f.write_str("yuv420p12le"),
            PixFmt::Yuv422p => f.write_str("yuv422p"),
            PixFmt::Yuv422p10le => f.write_str("yuv422p10le"),
            PixFmt::Yuv444p => f.write_str("yuv444p"),
            PixFmt::Yuv444p10le => f.write_str("yuv444p10le"),
            PixFmt::Yuvj420p => f.write_str("yuvj420p"),
            PixFmt::Yuvj422p => f.write_str("yuvj422p"),
            PixFmt::Yuvj444p => f.write_str("yuvj444p"),
            PixFmt::Other(s) => write!(f, "{s}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Field order
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldOrder {
    Progressive,
    InterlacedTopFirst,
    InterlacedBottomFirst,
    /// ffprobe didn't report it; treat as "probably progressive" downstream.
    Unknown,
}

impl FieldOrder {
    /// Parse ffprobe's `field_order` string.
    pub fn from_ffprobe_name(s: &str) -> Self {
        match s {
            "progressive" => FieldOrder::Progressive,
            "tt" | "tb" => FieldOrder::InterlacedTopFirst,
            "bb" | "bt" => FieldOrder::InterlacedBottomFirst,
            _ => FieldOrder::Unknown,
        }
    }

    pub fn is_interlaced(self) -> bool {
        matches!(
            self,
            FieldOrder::InterlacedTopFirst | FieldOrder::InterlacedBottomFirst
        )
    }
}

impl fmt::Display for FieldOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            FieldOrder::Progressive => "progressive",
            FieldOrder::InterlacedTopFirst => "interlaced (TFF)",
            FieldOrder::InterlacedBottomFirst => "interlaced (BFF)",
            FieldOrder::Unknown => "unknown field order",
        };
        f.write_str(s)
    }
}

// ---------------------------------------------------------------------------
// Rationals (frame rate, SAR)
// ---------------------------------------------------------------------------

/// A rational number as reported by ffprobe (`"30000/1001"`, `"1:1"`).
///
/// Zero denominators and non-numeric inputs parse to `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    pub fn new(num: u32, den: u32) -> Option<Self> {
        if den == 0 { None } else { Some(Self { num, den }) }
    }

    /// Parse `"n/d"` or `"n:d"`.
    pub fn parse(s: &str) -> Option<Self> {
        let (n, d) = s.split_once('/').or_else(|| s.split_once(':'))?;
        let num: u32 = n.trim().parse().ok()?;
        let den: u32 = d.trim().parse().ok()?;
        Self::new(num, den)
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            // Render frame rates as fractional decimals when common.
            let v = self.as_f64();
            if (v - v.round()).abs() < 1e-6 {
                write!(f, "{}", v.round() as u32)
            } else {
                write!(f, "{v:.3}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stream info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub field_order: FieldOrder,
    pub framerate: Option<Rational>,
    pub pix_fmt: Option<PixFmt>,
    pub sar: Option<Rational>,
    pub is_hdr: bool,
}

impl VideoInfo {
    /// Display aspect ratio derived from `width`, `height`, and SAR.
    pub fn display_size(&self) -> (u32, u32) {
        match self.sar {
            Some(sar) if sar.num != sar.den => {
                let w = (self.width as f64 * sar.as_f64()).round() as u32;
                (w, self.height)
            }
            _ => (self.width, self.height),
        }
    }

    pub fn has_square_pixels(&self) -> bool {
        matches!(self.sar, None | Some(Rational { num: 1, den: 1 }))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioInfo {
    pub codec: AudioCodec,
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub sample_rate_hz: Option<u32>,
    pub bitrate_bps: Option<u64>,
    pub language: Option<String>,
}

// ---------------------------------------------------------------------------
// MediaProfile (the result of step 1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaProfile {
    pub container: Container,
    pub video: VideoInfo,
    pub audio: Vec<AudioInfo>,
    pub duration_secs: Option<f64>,
    pub file_size_bytes: Option<u64>,
    pub bitrate_bps: Option<u64>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_prefers_mp4_over_mov() {
        assert_eq!(
            Container::from_ffprobe_name("mov,mp4,m4a,3gp,3g2,mj2"),
            Container::Mp4
        );
    }

    #[test]
    fn container_matroska_defaults_to_mkv() {
        assert_eq!(
            Container::from_ffprobe_name("matroska,webm"),
            Container::Mkv
        );
    }

    #[test]
    fn container_refines_webm_from_extension() {
        let c = Container::from_ffprobe_name("matroska,webm")
            .refine_with_filename(Path::new("clip.webm"));
        assert_eq!(c, Container::WebM);
    }

    #[test]
    fn container_refines_mkv_from_extension() {
        let c = Container::from_ffprobe_name("matroska,webm")
            .refine_with_filename(Path::new("movie.MKV")); // case-insensitive
        assert_eq!(c, Container::Mkv);
    }

    #[test]
    fn container_unknown_for_weird_format() {
        assert_eq!(
            Container::from_ffprobe_name("gxf,mxf"),
            Container::Unknown("gxf,mxf".to_string())
        );
    }

    #[test]
    fn video_codec_variants() {
        assert_eq!(VideoCodec::from_ffprobe_name("h264"), VideoCodec::H264);
        assert_eq!(VideoCodec::from_ffprobe_name("hevc"), VideoCodec::H265);
        assert_eq!(VideoCodec::from_ffprobe_name("h265"), VideoCodec::H265);
        assert_eq!(
            VideoCodec::from_ffprobe_name("mpeg2video"),
            VideoCodec::Mpeg2
        );
        assert_eq!(
            VideoCodec::from_ffprobe_name("msmpeg4v3"),
            VideoCodec::Msmpeg4
        );
        assert_eq!(
            VideoCodec::from_ffprobe_name("cinepak"),
            VideoCodec::Unknown("cinepak".to_string())
        );
    }

    #[test]
    fn audio_codec_groups_pcm_and_wma() {
        assert_eq!(
            AudioCodec::from_ffprobe_name("pcm_s16le"),
            AudioCodec::Pcm("pcm_s16le".to_string())
        );
        assert_eq!(
            AudioCodec::from_ffprobe_name("wmav2"),
            AudioCodec::Wma("wmav2".to_string())
        );
        assert_eq!(AudioCodec::from_ffprobe_name("opus"), AudioCodec::Opus);
    }

    #[test]
    fn pix_fmt_is_modern() {
        assert!(PixFmt::Yuv420p.is_modern());
        assert!(PixFmt::Yuv420p10le.is_modern());
        assert!(!PixFmt::Yuv422p.is_modern());
        assert!(!PixFmt::Yuvj420p.is_modern());
    }

    #[test]
    fn field_order_interlaced_detection() {
        assert!(FieldOrder::from_ffprobe_name("tt").is_interlaced());
        assert!(FieldOrder::from_ffprobe_name("bb").is_interlaced());
        assert!(!FieldOrder::from_ffprobe_name("progressive").is_interlaced());
        assert!(!FieldOrder::from_ffprobe_name("").is_interlaced()); // Unknown
    }

    #[test]
    fn rational_parses_slash_and_colon() {
        assert_eq!(Rational::parse("30000/1001"), Rational::new(30000, 1001));
        assert_eq!(Rational::parse("16:9"), Rational::new(16, 9));
        assert_eq!(Rational::parse("0/0"), None);
        assert_eq!(Rational::parse("garbage"), None);
    }

    #[test]
    fn video_info_display_size_accounts_for_sar() {
        let v = VideoInfo {
            codec: VideoCodec::Mpeg2,
            width: 720,
            height: 480,
            field_order: FieldOrder::InterlacedTopFirst,
            framerate: Rational::new(30000, 1001),
            pix_fmt: Some(PixFmt::Yuv420p),
            sar: Rational::new(10, 11),
            is_hdr: false,
        };
        // 720 * 10/11 ≈ 654.5 → 655 rounded
        assert_eq!(v.display_size(), (655, 480));
        assert!(!v.has_square_pixels());
    }
}
