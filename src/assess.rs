//! Step 4: pure assessment of whether a probed file is "fine as-is".
//!
//! A file is fine (refuse to touch without `--force`) iff all hold:
//!
//! - Video codec ∈ {H.264, H.265, AV1, VP9}
//! - Audio codec ∈ {AAC, Opus, FLAC, AC-3, E-AC-3} (AC-3/E-AC-3 only OK in MKV)
//! - Container ∈ {MP4, MKV, WebM}
//! - Progressive (not interlaced)
//! - Pixel format ∈ {yuv420p, yuv420p10le}
//! - Sample aspect ratio is 1:1 (square pixels)
//!
//! Several rules from the design doc are deferred to later build-order steps:
//!
//! - [`Issue::MissingSeekIndex`] (MP4 moov-at-end / MKV cues-missing) requires
//!   probing the container internals; not currently surfaced by ffprobe at
//!   the level we ingest.
//! - [`Issue::BitrateExcessive`] / [`Issue::ResolutionWasteful`] are the
//!   shrink-mode rules — step 5.
//! - [`Issue::AudioCodecIncompatibleWithTargetContainer`] depends on the
//!   user's chosen target container; computed in the planner, not here.
//!
//! Their variants are kept in the public enum so the API stays stable as
//! later steps fill them in.

use crate::model::{AudioCodec, Container, MediaProfile, PixFmt, VideoCodec};

/// A single thing that makes a file "not fine".
///
/// Variants annotated `// step-N` are intentionally not produced by [`assess`]
/// yet; they live here so consumers can `match` exhaustively without churn
/// when later build-order steps wire them up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    LegacyVideoCodec(VideoCodec),
    LegacyAudioCodec(AudioCodec),
    LegacyContainer(Container),
    /// MP4 with moov at end, MKV without Cues, AVI, MpegPs, etc. — step 7.
    #[allow(dead_code)]
    MissingSeekIndex,
    Interlaced,
    /// Pixel format not in {yuv420p, yuv420p10le}.
    NonModernPixelFormat(PixFmt),
    /// Shrink-mode signal — step 5.
    #[allow(dead_code)]
    BitrateExcessive { actual: u64, threshold: u64 },
    /// e.g. 4K source with already-low bitrate-per-pixel — step 5.
    #[allow(dead_code)]
    ResolutionWasteful,
    NonSquarePixels { sar_num: u32, sar_den: u32 },
    /// Step 5+ (planner-time, depends on target container).
    #[allow(dead_code)]
    AudioCodecIncompatibleWithTargetContainer,
}

/// Result of [`assess`] over a [`MediaProfile`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assessment {
    pub issues: Vec<Issue>,
}

impl Assessment {
    /// A file is "fine" (don't touch without `--force`) iff no issues were found.
    pub fn is_fine(&self) -> bool {
        self.issues.is_empty()
    }

    /// True iff at least one issue can only be fixed by re-encoding video.
    ///
    /// Container-only issues ([`Issue::LegacyContainer`], [`Issue::LegacyAudioCodec`])
    /// can be resolved by remuxing alone; everything else demands a re-encode.
    /// Used by the `Intent::Auto` planner to pick between remux and re-encode.
    pub fn requires_reencode(&self) -> bool {
        self.issues.iter().any(|i| match i {
            Issue::LegacyVideoCodec(_)
            | Issue::Interlaced
            | Issue::NonModernPixelFormat(_)
            | Issue::NonSquarePixels { .. }
            | Issue::BitrateExcessive { .. }
            | Issue::ResolutionWasteful => true,
            Issue::LegacyContainer(_)
            | Issue::LegacyAudioCodec(_)
            | Issue::MissingSeekIndex
            | Issue::AudioCodecIncompatibleWithTargetContainer => false,
        })
    }
}

/// Run every "is this file fine?" check against `profile`.
///
/// Pure: no I/O, no allocation beyond the returned [`Assessment`].
pub fn assess(profile: &MediaProfile) -> Assessment {
    let mut issues = Vec::new();

    if !is_modern_video_codec(&profile.video.codec, &profile.container) {
        issues.push(Issue::LegacyVideoCodec(profile.video.codec.clone()));
    }

    if !is_modern_container(&profile.container) {
        issues.push(Issue::LegacyContainer(profile.container.clone()));
    }

    // Audio: at least one track must be present, and every track must be a
    // codec we'd happily distribute. We flag the *first* legacy track only,
    // to keep the issues list short — the user just needs to know there's
    // an audio problem, not enumerate every track.
    if let Some(legacy) = profile
        .audio
        .iter()
        .find(|a| !is_modern_audio_codec(&a.codec, &profile.container))
    {
        issues.push(Issue::LegacyAudioCodec(legacy.codec.clone()));
    }

    if profile.video.field_order.is_interlaced() {
        issues.push(Issue::Interlaced);
    }

    if let Some(ref pf) = profile.video.pix_fmt {
        if !pf.is_modern() {
            issues.push(Issue::NonModernPixelFormat(pf.clone()));
        }
    }

    // Square-pixel check: SAR present and != 1:1. Absent SAR is treated as
    // "probably square" since most modern files don't bother encoding it.
    if let Some(sar) = profile.video.sar {
        if sar.num != sar.den {
            issues.push(Issue::NonSquarePixels {
                sar_num: sar.num,
                sar_den: sar.den,
            });
        }
    }

    Assessment { issues }
}

/// True for video codecs we'd consider "modern enough" given the container.
///
/// H.264 / H.265 / AV1 / VP9 are universally fine. VP8 is fine in WebM and
/// MKV (the canonical homes for VP8) but should be transcoded for MP4 (which
/// doesn't officially carry VP8 anyway). The point of the container guard
/// is to avoid useless lossy generation-loss re-encodes of files that are
/// already a complete, web-compatible package.
fn is_modern_video_codec(codec: &VideoCodec, container: &Container) -> bool {
    match codec {
        VideoCodec::H264 | VideoCodec::H265 | VideoCodec::Av1 | VideoCodec::Vp9 => true,
        VideoCodec::Vp8 => matches!(container, Container::Mkv | Container::WebM),
        _ => false,
    }
}

/// True for containers we'd happily produce.
fn is_modern_container(container: &Container) -> bool {
    matches!(
        container,
        Container::Mp4 | Container::Mkv | Container::WebM
    )
}

/// True for audio codecs that don't require a re-encode in the given container.
///
/// AAC / Opus / FLAC are universally fine. AC-3 / E-AC-3 are fine in MKV
/// (commonly preserved from DVD/Blu-ray sources) but legacy in MP4. Vorbis
/// is fine in WebM/MKV (its native homes) but legacy elsewhere.
fn is_modern_audio_codec(codec: &AudioCodec, container: &Container) -> bool {
    match codec {
        AudioCodec::Aac | AudioCodec::Opus | AudioCodec::Flac => true,
        AudioCodec::Ac3 | AudioCodec::Eac3 => matches!(container, Container::Mkv),
        AudioCodec::Vorbis => matches!(container, Container::Mkv | Container::WebM),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AudioInfo, FieldOrder, MediaProfile, Rational, VideoCodec, VideoInfo,
    };
    use pretty_assertions::assert_eq;

    /// Modern, perfectly-fine baseline: H.264 1080p in MP4 with AAC stereo.
    /// All assess() rules pass.
    fn fine_profile() -> MediaProfile {
        MediaProfile {
            container: Container::Mp4,
            video: VideoInfo {
                codec: VideoCodec::H264,
                width: 1920,
                height: 1080,
                field_order: FieldOrder::Progressive,
                framerate: Rational::new(24, 1),
                pix_fmt: Some(PixFmt::Yuv420p),
                sar: Rational::new(1, 1),
                is_hdr: false,
            },
            audio: vec![AudioInfo {
                codec: AudioCodec::Aac,
                channels: Some(2),
                channel_layout: Some("stereo".into()),
                sample_rate_hz: Some(48_000),
                bitrate_bps: Some(192_000),
                language: Some("eng".into()),
            }],
            duration_secs: Some(60.0),
            file_size_bytes: Some(50_000_000),
            bitrate_bps: Some(5_000_000),
        }
    }

    #[test]
    fn modern_h264_mp4_is_fine() {
        let a = assess(&fine_profile());
        assert!(a.is_fine(), "expected fine, got issues: {:?}", a.issues);
        assert!(!a.requires_reencode());
    }

    #[test]
    fn modern_av1_webm_with_opus_is_fine() {
        let mut p = fine_profile();
        p.container = Container::WebM;
        p.video.codec = VideoCodec::Av1;
        p.audio[0].codec = AudioCodec::Opus;
        assert!(assess(&p).is_fine());
    }

    #[test]
    fn legacy_video_codec_flagged_and_requires_reencode() {
        let mut p = fine_profile();
        p.video.codec = VideoCodec::Mpeg2;
        let a = assess(&p);
        assert_eq!(a.issues, vec![Issue::LegacyVideoCodec(VideoCodec::Mpeg2)]);
        assert!(a.requires_reencode());
    }

    #[test]
    fn legacy_container_alone_is_remux_only() {
        // Modern H.264 / AAC in an AVI container: remuxing to MKV is enough.
        let mut p = fine_profile();
        p.container = Container::Avi;
        let a = assess(&p);
        assert_eq!(a.issues, vec![Issue::LegacyContainer(Container::Avi)]);
        assert!(!a.requires_reencode());
    }

    #[test]
    fn vp8_in_webm_is_fine() {
        // The Diablo III WebM case: VP8 + Vorbis in WebM is a complete,
        // web-compatible package. Re-encoding would just be lossy generation
        // loss for no benefit.
        let mut p = fine_profile();
        p.container = Container::WebM;
        p.video.codec = VideoCodec::Vp8;
        p.audio[0].codec = AudioCodec::Vorbis;
        assert!(
            assess(&p).is_fine(),
            "VP8 + Vorbis in WebM should be fine, got: {:?}",
            assess(&p).issues
        );
    }

    #[test]
    fn vp8_in_mkv_is_fine() {
        let mut p = fine_profile();
        p.container = Container::Mkv;
        p.video.codec = VideoCodec::Vp8;
        assert!(assess(&p).is_fine());
    }

    #[test]
    fn vp8_in_mp4_is_legacy() {
        // MP4 doesn't officially carry VP8, so transcode is appropriate.
        let mut p = fine_profile();
        p.container = Container::Mp4;
        p.video.codec = VideoCodec::Vp8;
        let a = assess(&p);
        assert!(a.issues.contains(&Issue::LegacyVideoCodec(VideoCodec::Vp8)));
        assert!(a.requires_reencode());
    }

    #[test]
    fn vorbis_in_webm_is_fine() {
        let mut p = fine_profile();
        p.container = Container::WebM;
        p.video.codec = VideoCodec::Vp9; // pair Vorbis with a non-VP8 codec
        p.audio[0].codec = AudioCodec::Vorbis;
        assert!(assess(&p).is_fine());
    }

    #[test]
    fn vorbis_in_mp4_is_legacy() {
        let mut p = fine_profile();
        p.container = Container::Mp4;
        p.audio[0].codec = AudioCodec::Vorbis;
        let a = assess(&p);
        assert!(
            a.issues.contains(&Issue::LegacyAudioCodec(AudioCodec::Vorbis))
        );
    }

    #[test]
    fn ac3_in_mkv_is_fine_but_ac3_in_mp4_is_not() {
        let mut p = fine_profile();
        p.container = Container::Mkv;
        p.audio[0].codec = AudioCodec::Ac3;
        assert!(assess(&p).is_fine(), "AC-3 in MKV should be fine");

        p.container = Container::Mp4;
        let a = assess(&p);
        assert!(
            a.issues.contains(&Issue::LegacyAudioCodec(AudioCodec::Ac3)),
            "AC-3 in MP4 should be flagged: {:?}",
            a.issues
        );
        assert!(!a.requires_reencode(), "audio-only fix is remux-able");
    }

    #[test]
    fn legacy_audio_codec_alone_is_remux_only() {
        // MP3 in MKV: legacy audio, but remux to MKV (which already accepts
        // MP3) is the right move and keeps everything as stream-copy.
        let mut p = fine_profile();
        p.container = Container::Mkv;
        p.audio[0].codec = AudioCodec::Mp3;
        let a = assess(&p);
        assert_eq!(a.issues, vec![Issue::LegacyAudioCodec(AudioCodec::Mp3)]);
        assert!(!a.requires_reencode());
    }

    #[test]
    fn interlaced_source_is_flagged_and_requires_reencode() {
        let mut p = fine_profile();
        p.video.field_order = FieldOrder::InterlacedTopFirst;
        let a = assess(&p);
        assert!(a.issues.contains(&Issue::Interlaced));
        assert!(a.requires_reencode());
    }

    #[test]
    fn non_modern_pix_fmt_is_flagged_and_requires_reencode() {
        let mut p = fine_profile();
        p.video.pix_fmt = Some(PixFmt::Yuvj420p);
        let a = assess(&p);
        assert!(
            a.issues
                .contains(&Issue::NonModernPixelFormat(PixFmt::Yuvj420p))
        );
        assert!(a.requires_reencode());
    }

    #[test]
    fn yuv420p10le_is_modern() {
        let mut p = fine_profile();
        p.video.codec = VideoCodec::H265;
        p.video.pix_fmt = Some(PixFmt::Yuv420p10le);
        assert!(assess(&p).is_fine());
    }

    #[test]
    fn non_square_pixels_flagged_and_requires_reencode() {
        let mut p = fine_profile();
        p.video.sar = Rational::new(10, 11);
        let a = assess(&p);
        assert!(
            a.issues
                .contains(&Issue::NonSquarePixels { sar_num: 10, sar_den: 11 })
        );
        assert!(a.requires_reencode());
    }

    #[test]
    fn missing_sar_is_treated_as_square() {
        let mut p = fine_profile();
        p.video.sar = None;
        assert!(assess(&p).is_fine());
    }

    #[test]
    fn dvd_vob_accumulates_every_relevant_issue() {
        // Classic DVD rip: MPEG-2 720x480 interlaced 10:11 SAR in MPEG-PS
        // with AC-3 audio. Should yield: legacy codec, legacy container,
        // interlaced, non-square pixels (AC-3 in MPEG-PS is also "legacy"
        // since the container itself is rejected, AC-3 is fine in MKV).
        let p = MediaProfile {
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
                channel_layout: Some("5.1".into()),
                sample_rate_hz: Some(48_000),
                bitrate_bps: Some(448_000),
                language: None,
            }],
            duration_secs: Some(5520.0),
            file_size_bytes: Some(4_500_000_000),
            bitrate_bps: Some(6_500_000),
        };
        let a = assess(&p);
        assert!(a.issues.contains(&Issue::LegacyVideoCodec(VideoCodec::Mpeg2)));
        assert!(a.issues.contains(&Issue::LegacyContainer(Container::MpegPs)));
        assert!(a.issues.contains(&Issue::Interlaced));
        assert!(
            a.issues
                .contains(&Issue::NonSquarePixels { sar_num: 10, sar_den: 11 })
        );
        // AC-3 in *MpegPs* is a legacy audio codec because AC-3 is only
        // fine in MKV containers.
        assert!(a.issues.contains(&Issue::LegacyAudioCodec(AudioCodec::Ac3)));
        assert!(a.requires_reencode());
    }

    #[test]
    fn empty_audio_track_list_is_not_an_audio_issue() {
        // Silent video. assess() shouldn't care; it just checks the tracks
        // that exist.
        let mut p = fine_profile();
        p.audio.clear();
        assert!(assess(&p).is_fine());
    }
}
