//! Step 1 of the pipeline: run `ffprobe` and build a typed [`MediaProfile`].
//!
//! The ffprobe invocation is fixed:
//!
//! ```text
//! ffprobe -v error -print_format json -show_streams -show_format <path>
//! ```
//!
//! The resulting JSON is parsed via a pair of internal "raw" serde structs that
//! mirror ffprobe's actual output (where most fields are optional, even ones
//! that *should* always be present), and then converted into the crate's own
//! domain types in [`crate::model`].
//!
//! The binary path can be overridden via the `VIMPROVER_FFPROBE` environment
//! variable. Otherwise `which` searches `$PATH`.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Deserialize;
use tokio::process::Command;
use tracing::debug;

use crate::error::{Error, Result};
use crate::model::{
    AudioCodec, AudioInfo, Container, FieldOrder, MediaProfile, PixFmt, Rational, VideoCodec,
    VideoInfo,
};

/// Locate the ffprobe binary, preferring the `VIMPROVER_FFPROBE` env override.
pub fn locate_ffprobe() -> Result<PathBuf> {
    if let Ok(override_path) = env::var("VIMPROVER_FFPROBE") {
        if !override_path.is_empty() {
            return Ok(PathBuf::from(override_path));
        }
    }
    which::which("ffprobe").map_err(Error::FfprobeNotFound)
}

/// Probe a single media file and return a [`MediaProfile`].
///
/// Errors if ffprobe isn't installed, the file is missing, ffprobe fails, the
/// JSON doesn't parse, or the file contains no video stream.
pub async fn probe_file(path: &Path) -> Result<MediaProfile> {
    if !path.exists() {
        return Err(Error::InputNotFound(path.to_path_buf()));
    }
    if !path.is_file() {
        return Err(Error::InputNotFile(path.to_path_buf()));
    }

    let ffprobe = locate_ffprobe()?;
    debug!(?ffprobe, ?path, "spawning ffprobe");

    let output = Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        return Err(Error::FfprobeFailed {
            path: path.to_path_buf(),
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    parse_ffprobe_json(path, &output.stdout)
}

/// Pure function: parse raw ffprobe JSON bytes into a `MediaProfile`.
///
/// Exposed as `pub(crate)` so tests can feed in fixtures without running
/// ffprobe.
pub(crate) fn parse_ffprobe_json(path: &Path, bytes: &[u8]) -> Result<MediaProfile> {
    let raw: RawOutput =
        serde_json::from_slice(bytes).map_err(|source| Error::FfprobeParse {
            path: path.to_path_buf(),
            source,
        })?;
    build_profile(path, raw)
}

fn build_profile(path: &Path, raw: RawOutput) -> Result<MediaProfile> {
    if raw.streams.is_empty() {
        return Err(Error::NoStreams(path.to_path_buf()));
    }

    let video = raw
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"))
        .map(build_video_info)
        .ok_or_else(|| Error::NoVideoStream(path.to_path_buf()))?;

    let audio: Vec<AudioInfo> = raw
        .streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("audio"))
        .map(build_audio_info)
        .collect();

    let format = raw.format.unwrap_or_default();

    let container = format
        .format_name
        .as_deref()
        .map(Container::from_ffprobe_name)
        .unwrap_or_else(|| Container::Unknown(String::from("(missing format_name)")))
        .refine_with_filename(path);

    let duration_secs = format
        .duration
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|f| f.is_finite() && *f >= 0.0);

    let file_size_bytes = format.size.as_deref().and_then(|s| s.parse::<u64>().ok());

    let bitrate_bps = format
        .bit_rate
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok());

    Ok(MediaProfile {
        container,
        video,
        audio,
        duration_secs,
        file_size_bytes,
        bitrate_bps,
    })
}

fn build_video_info(s: &RawStream) -> VideoInfo {
    let codec = s
        .codec_name
        .as_deref()
        .map(VideoCodec::from_ffprobe_name)
        .unwrap_or_else(|| VideoCodec::Unknown(String::from("(missing codec_name)")));
    let width = s.width.unwrap_or(0);
    let height = s.height.unwrap_or(0);
    let field_order = s
        .field_order
        .as_deref()
        .map(FieldOrder::from_ffprobe_name)
        .unwrap_or(FieldOrder::Unknown);
    let framerate = s
        .r_frame_rate
        .as_deref()
        .or(s.avg_frame_rate.as_deref())
        .and_then(Rational::parse);
    let pix_fmt = s.pix_fmt.as_deref().map(PixFmt::from_ffprobe_name);
    let sar = s.sample_aspect_ratio.as_deref().and_then(Rational::parse);
    let is_hdr = matches!(
        s.color_transfer.as_deref(),
        Some("smpte2084") | Some("arib-std-b67")
    );

    VideoInfo {
        codec,
        width,
        height,
        field_order,
        framerate,
        pix_fmt,
        sar,
        is_hdr,
    }
}

fn build_audio_info(s: &RawStream) -> AudioInfo {
    let codec = s
        .codec_name
        .as_deref()
        .map(AudioCodec::from_ffprobe_name)
        .unwrap_or_else(|| AudioCodec::Unknown(String::from("(missing codec_name)")));
    let sample_rate_hz = s
        .sample_rate
        .as_deref()
        .and_then(|s| s.parse::<u32>().ok());
    let bitrate_bps = s.bit_rate.as_deref().and_then(|s| s.parse::<u64>().ok());
    let language = s.tags.get("language").or(s.tags.get("LANGUAGE")).cloned();

    AudioInfo {
        codec,
        channels: s.channels,
        channel_layout: s.channel_layout.clone(),
        sample_rate_hz,
        bitrate_bps,
        language,
    }
}

// ---------------------------------------------------------------------------
// Raw ffprobe JSON structs. Every field is optional because real-world weird
// files (and older ffprobe versions) omit things you'd expect to be present.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct RawOutput {
    #[serde(default)]
    streams: Vec<RawStream>,
    #[serde(default)]
    format: Option<RawFormat>,
}

#[derive(Debug, Default, Deserialize)]
struct RawStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    field_order: Option<String>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    sample_aspect_ratio: Option<String>,
    color_transfer: Option<String>,
    channels: Option<u32>,
    channel_layout: Option<String>,
    sample_rate: Option<String>,
    bit_rate: Option<String>,
    #[serde(default)]
    tags: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawFormat {
    format_name: Option<String>,
    duration: Option<String>,
    size: Option<String>,
    bit_rate: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // Representative DVD NTSC VOB: MPEG-2 720x480 interlaced 10:11 SAR, AC-3 5.1.
    const DVD_VOB_JSON: &str = r#"{
      "streams": [
        {
          "index": 0,
          "codec_name": "mpeg2video",
          "codec_type": "video",
          "width": 720,
          "height": 480,
          "pix_fmt": "yuv420p",
          "field_order": "tt",
          "r_frame_rate": "30000/1001",
          "avg_frame_rate": "30000/1001",
          "sample_aspect_ratio": "10:11",
          "display_aspect_ratio": "4:3",
          "color_transfer": "bt709"
        },
        {
          "index": 1,
          "codec_name": "ac3",
          "codec_type": "audio",
          "sample_rate": "48000",
          "channels": 6,
          "channel_layout": "5.1(side)",
          "bit_rate": "448000",
          "tags": { "language": "eng" }
        },
        {
          "index": 2,
          "codec_name": "dvd_subtitle",
          "codec_type": "subtitle"
        }
      ],
      "format": {
        "filename": "movie.vob",
        "format_name": "mpeg",
        "duration": "5520.360000",
        "size": "4509876543",
        "bit_rate": "6534220"
      }
    }"#;

    #[test]
    fn parses_dvd_vob_profile() {
        let p = Path::new("movie.vob");
        let prof = parse_ffprobe_json(p, DVD_VOB_JSON.as_bytes()).expect("parse");

        assert_eq!(prof.container, Container::MpegPs);
        assert_eq!(prof.video.codec, VideoCodec::Mpeg2);
        assert_eq!(prof.video.width, 720);
        assert_eq!(prof.video.height, 480);
        assert_eq!(prof.video.field_order, FieldOrder::InterlacedTopFirst);
        assert_eq!(prof.video.framerate, Rational::new(30000, 1001));
        assert_eq!(prof.video.pix_fmt, Some(PixFmt::Yuv420p));
        assert_eq!(prof.video.sar, Rational::new(10, 11));
        assert!(!prof.video.is_hdr);

        assert_eq!(prof.audio.len(), 1); // subtitle stream ignored
        let a = &prof.audio[0];
        assert_eq!(a.codec, AudioCodec::Ac3);
        assert_eq!(a.channels, Some(6));
        assert_eq!(a.channel_layout.as_deref(), Some("5.1(side)"));
        assert_eq!(a.bitrate_bps, Some(448_000));
        assert_eq!(a.language.as_deref(), Some("eng"));

        assert_eq!(prof.duration_secs, Some(5520.360000));
        assert_eq!(prof.file_size_bytes, Some(4_509_876_543));
        assert_eq!(prof.bitrate_bps, Some(6_534_220));
    }

    #[test]
    fn detects_hdr_via_color_transfer() {
        let json = r#"{
          "streams": [{
            "codec_name": "hevc",
            "codec_type": "video",
            "width": 3840,
            "height": 2160,
            "pix_fmt": "yuv420p10le",
            "field_order": "progressive",
            "r_frame_rate": "24/1",
            "sample_aspect_ratio": "1:1",
            "color_transfer": "smpte2084"
          }],
          "format": { "format_name": "matroska,webm" }
        }"#;
        let prof = parse_ffprobe_json(Path::new("x.mkv"), json.as_bytes()).unwrap();
        assert!(prof.video.is_hdr);
        assert_eq!(prof.container, Container::Mkv);
    }

    #[test]
    fn errors_without_streams() {
        let json = r#"{ "streams": [], "format": { "format_name": "mp4" } }"#;
        let err = parse_ffprobe_json(Path::new("x"), json.as_bytes()).unwrap_err();
        assert!(matches!(err, Error::NoStreams(_)));
    }

    #[test]
    fn errors_without_video_stream() {
        let json = r#"{
          "streams": [{ "codec_type": "audio", "codec_name": "mp3" }],
          "format": { "format_name": "mp3" }
        }"#;
        let err = parse_ffprobe_json(Path::new("x"), json.as_bytes()).unwrap_err();
        assert!(matches!(err, Error::NoVideoStream(_)));
    }

    #[test]
    fn tolerates_missing_optional_fields() {
        // Minimal JSON: just a video stream with no format block.
        let json = r#"{
          "streams": [{
            "codec_type": "video",
            "codec_name": "h264",
            "width": 1920,
            "height": 1080
          }]
        }"#;
        let prof = parse_ffprobe_json(Path::new("x.mp4"), json.as_bytes()).unwrap();
        assert_eq!(prof.video.codec, VideoCodec::H264);
        assert_eq!(prof.video.field_order, FieldOrder::Unknown);
        assert!(prof.video.framerate.is_none());
        assert!(prof.video.pix_fmt.is_none());
        assert!(prof.audio.is_empty());
        assert!(prof.duration_secs.is_none());
        assert!(matches!(prof.container, Container::Unknown(_)));
    }
}
