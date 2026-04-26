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
//! Step 5 adds intent-aware rules: when [`crate::plan::Intent::Shrink`] is
//! passed, [`assess`] additionally flags [`Issue::BitrateExcessive`] and
//! [`Issue::ResolutionWasteful`]. These rules don't fire under `Auto` or any
//! other intent because they describe "could be smaller", not "is broken".
//!
//! The remaining design-doc rules are still deferred:
//!
//! - [`Issue::MissingSeekIndex`] (MP4 moov-at-end / MKV cues-missing) requires
//!   probing the container internals; not currently surfaced by ffprobe at
//!   the level we ingest.
//! - [`Issue::AudioCodecIncompatibleWithTargetContainer`] depends on the
//!   user's chosen target container; computed in the planner, not here.
//!
//! Their variants are kept in the public enum so the API stays stable as
//! later steps fill them in.

use crate::model::{AudioCodec, Container, MediaProfile, PixFmt, VideoCodec};
use crate::plan::Intent;

/// A single thing that makes a file "not fine".
///
/// Variants annotated `// step-N` are intentionally not produced by [`assess`]
/// yet; they live here so consumers can `match` exhaustively without churn
/// when later build-order steps wire them up.
///
/// `Eq` is intentionally not derived: [`Issue::ResolutionWasteful`] carries
/// an `f64` (bits-per-pixel-per-second), which doesn't admit a total
/// equality. `PartialEq` is sufficient for our `assert_eq!` test usage.
#[derive(Debug, Clone, PartialEq)]
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
    /// Source bitrate is well above the per-resolution threshold; only fires
    /// in [`Intent::Shrink`] mode (where it's actionable).
    BitrateExcessive { actual: u64, threshold: u64 },
    /// e.g. 4K source already encoded at low bits-per-pixel — downscaling
    /// to a smaller resolution wouldn't lose meaningful detail. Only fires
    /// in [`Intent::Shrink`] mode.
    ResolutionWasteful { height: u32, bits_per_pixel_per_sec: f64 },
    NonSquarePixels { sar_num: u32, sar_den: u32 },
    /// Step 5+ (planner-time, depends on target container).
    #[allow(dead_code)]
    AudioCodecIncompatibleWithTargetContainer,
}

/// Result of [`assess`] over a [`MediaProfile`].
#[derive(Debug, Clone, Default, PartialEq)]
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
            | Issue::ResolutionWasteful { .. } => true,
            Issue::LegacyContainer(_)
            | Issue::LegacyAudioCodec(_)
            | Issue::MissingSeekIndex
            | Issue::AudioCodecIncompatibleWithTargetContainer => false,
        })
    }
}

/// Run every "is this file fine?" check against `profile`, conditioned on
/// the user's [`Intent`].
///
/// Pure: no I/O, no allocation beyond the returned [`Assessment`].
///
/// Most rules (modern codec/container/pixel-format/etc.) fire regardless of
/// intent — a broken file is broken whatever the user wants. The bitrate /
/// resolution rules only fire under [`Intent::Shrink`], because "this file
/// could be smaller" is only actionable when the user has asked us to make
/// it smaller.
pub fn assess(profile: &MediaProfile, intent: &Intent) -> Assessment {
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

    // Shrink-mode-only rules. We only care whether a file *could* be smaller
    // when the user has explicitly asked us to shrink it.
    if matches!(intent, Intent::Shrink { .. }) {
        if let Some(actual) = profile.bitrate_bps {
            let threshold = bitrate_threshold_for_height(profile.video.height);
            if actual > threshold {
                issues.push(Issue::BitrateExcessive { actual, threshold });
            }

            // Heuristic: at 1440p+ with very few bits per pixel, the encoder
            // already isn't capturing much detail, so a smaller resolution
            // would look essentially identical.
            if profile.video.height >= 1440 {
                let pixels = (profile.video.width as f64) * (profile.video.height as f64);
                if pixels > 0.0 {
                    let bpp = (actual as f64) / pixels;
                    if bpp < WASTEFUL_BPP_THRESHOLD {
                        issues.push(Issue::ResolutionWasteful {
                            height: profile.video.height,
                            bits_per_pixel_per_sec: bpp,
                        });
                    }
                }
            }
        }
    }

    Assessment { issues }
}

/// "Reasonable" upper bitrate (in bits/sec) for a given encoded height. Any
/// source above this threshold is a candidate for re-encoding smaller. Used
/// both by [`assess`] (to flag [`Issue::BitrateExcessive`]) and by the
/// planner (to derive a default shrink target when the user passes
/// `--intent shrink` with no explicit `--target-bitrate`).
///
/// Numbers chosen to be comfortably above modern x264/x265 streaming targets
/// (Netflix, YouTube high-quality) so we don't flag legitimately good
/// encodes.
pub fn bitrate_threshold_for_height(height: u32) -> u64 {
    if height <= 480 {
        1_500_000
    } else if height <= 720 {
        3_000_000
    } else if height <= 1080 {
        5_000_000
    } else if height <= 1440 {
        9_000_000
    } else {
        15_000_000
    }
}

/// Bits-per-pixel-per-second below which a high-resolution source is
/// considered "wasteful": the encoder already isn't using much information
/// per pixel, so downscaling won't lose meaningful detail.
const WASTEFUL_BPP_THRESHOLD: f64 = 1.5;

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

    /// Default intent for the existing rule-coverage tests: most rules don't
    /// depend on intent, so `Auto` is fine. Shrink-mode-only tests use
    /// `Intent::Shrink { .. }` directly.
    const AUTO: &Intent = &Intent::Auto;

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
        let a = assess(&fine_profile(), AUTO);
        assert!(a.is_fine(), "expected fine, got issues: {:?}", a.issues);
        assert!(!a.requires_reencode());
    }

    #[test]
    fn modern_av1_webm_with_opus_is_fine() {
        let mut p = fine_profile();
        p.container = Container::WebM;
        p.video.codec = VideoCodec::Av1;
        p.audio[0].codec = AudioCodec::Opus;
        assert!(assess(&p, AUTO).is_fine());
    }

    #[test]
    fn legacy_video_codec_flagged_and_requires_reencode() {
        let mut p = fine_profile();
        p.video.codec = VideoCodec::Mpeg2;
        let a = assess(&p, AUTO);
        assert_eq!(a.issues, vec![Issue::LegacyVideoCodec(VideoCodec::Mpeg2)]);
        assert!(a.requires_reencode());
    }

    #[test]
    fn legacy_container_alone_is_remux_only() {
        // Modern H.264 / AAC in an AVI container: remuxing to MKV is enough.
        let mut p = fine_profile();
        p.container = Container::Avi;
        let a = assess(&p, AUTO);
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
            assess(&p, AUTO).is_fine(),
            "VP8 + Vorbis in WebM should be fine, got: {:?}",
            assess(&p, AUTO).issues
        );
    }

    #[test]
    fn vp8_in_mkv_is_fine() {
        let mut p = fine_profile();
        p.container = Container::Mkv;
        p.video.codec = VideoCodec::Vp8;
        assert!(assess(&p, AUTO).is_fine());
    }

    #[test]
    fn vp8_in_mp4_is_legacy() {
        // MP4 doesn't officially carry VP8, so transcode is appropriate.
        let mut p = fine_profile();
        p.container = Container::Mp4;
        p.video.codec = VideoCodec::Vp8;
        let a = assess(&p, AUTO);
        assert!(a.issues.contains(&Issue::LegacyVideoCodec(VideoCodec::Vp8)));
        assert!(a.requires_reencode());
    }

    #[test]
    fn vorbis_in_webm_is_fine() {
        let mut p = fine_profile();
        p.container = Container::WebM;
        p.video.codec = VideoCodec::Vp9; // pair Vorbis with a non-VP8 codec
        p.audio[0].codec = AudioCodec::Vorbis;
        assert!(assess(&p, AUTO).is_fine());
    }

    #[test]
    fn vorbis_in_mp4_is_legacy() {
        let mut p = fine_profile();
        p.container = Container::Mp4;
        p.audio[0].codec = AudioCodec::Vorbis;
        let a = assess(&p, AUTO);
        assert!(
            a.issues.contains(&Issue::LegacyAudioCodec(AudioCodec::Vorbis))
        );
    }

    #[test]
    fn ac3_in_mkv_is_fine_but_ac3_in_mp4_is_not() {
        let mut p = fine_profile();
        p.container = Container::Mkv;
        p.audio[0].codec = AudioCodec::Ac3;
        assert!(assess(&p, AUTO).is_fine(), "AC-3 in MKV should be fine");

        p.container = Container::Mp4;
        let a = assess(&p, AUTO);
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
        let a = assess(&p, AUTO);
        assert_eq!(a.issues, vec![Issue::LegacyAudioCodec(AudioCodec::Mp3)]);
        assert!(!a.requires_reencode());
    }

    #[test]
    fn interlaced_source_is_flagged_and_requires_reencode() {
        let mut p = fine_profile();
        p.video.field_order = FieldOrder::InterlacedTopFirst;
        let a = assess(&p, AUTO);
        assert!(a.issues.contains(&Issue::Interlaced));
        assert!(a.requires_reencode());
    }

    #[test]
    fn non_modern_pix_fmt_is_flagged_and_requires_reencode() {
        let mut p = fine_profile();
        p.video.pix_fmt = Some(PixFmt::Yuvj420p);
        let a = assess(&p, AUTO);
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
        assert!(assess(&p, AUTO).is_fine());
    }

    #[test]
    fn non_square_pixels_flagged_and_requires_reencode() {
        let mut p = fine_profile();
        p.video.sar = Rational::new(10, 11);
        let a = assess(&p, AUTO);
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
        assert!(assess(&p, AUTO).is_fine());
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
        let a = assess(&p, AUTO);
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
        assert!(assess(&p, AUTO).is_fine());
    }

    // -- Shrink-mode rules ---------------------------------------------------

    /// Empty Shrink intent for tests: no caps from the user.
    const SHRINK: &Intent = &Intent::Shrink {
        max_height: None,
        target_bitrate_bps: None,
    };

    #[test]
    fn bitrate_excessive_only_fires_under_shrink() {
        // 1080p at 12 Mbps: well above the 5 Mbps threshold.
        let mut p = fine_profile();
        p.bitrate_bps = Some(12_000_000);

        // Auto: no shrink-mode rules; file is fine.
        assert!(assess(&p, AUTO).is_fine());

        // Shrink: flagged.
        let a = assess(&p, SHRINK);
        assert!(
            a.issues.contains(&Issue::BitrateExcessive {
                actual: 12_000_000,
                threshold: 5_000_000,
            }),
            "expected BitrateExcessive under Shrink: {:?}",
            a.issues
        );
        assert!(a.requires_reencode());
    }

    #[test]
    fn bitrate_at_or_below_threshold_is_not_excessive() {
        let mut p = fine_profile();
        p.bitrate_bps = Some(5_000_000); // exactly at the 1080p threshold
        assert!(assess(&p, SHRINK).is_fine());

        p.bitrate_bps = Some(2_500_000); // well below
        assert!(assess(&p, SHRINK).is_fine());
    }

    #[test]
    fn missing_bitrate_does_not_trigger_shrink_rules() {
        // ffprobe occasionally fails to report bitrate (e.g. malformed
        // streams). assess() should silently skip the bitrate-based rules.
        let mut p = fine_profile();
        p.bitrate_bps = None;
        assert!(assess(&p, SHRINK).is_fine());
    }

    #[test]
    fn resolution_wasteful_fires_for_4k_with_low_bpp() {
        // 4K (2160p) at 6 Mbps: 6e6 / (3840*2160) ≈ 0.72 bps/pixel — well
        // below the 1.5 threshold, so flagged as wasteful. Also above the
        // 4K bitrate threshold (15 Mbps)? No — 6 Mbps is below 15 Mbps, so
        // no BitrateExcessive. Just ResolutionWasteful.
        let mut p = fine_profile();
        p.video.width = 3840;
        p.video.height = 2160;
        p.bitrate_bps = Some(6_000_000);

        let a = assess(&p, SHRINK);
        let wasteful = a.issues.iter().any(|i| {
            matches!(i, Issue::ResolutionWasteful { height, .. } if *height == 2160)
        });
        assert!(wasteful, "expected ResolutionWasteful: {:?}", a.issues);
        assert!(!a.issues.iter().any(|i| matches!(i, Issue::BitrateExcessive { .. })));
    }

    #[test]
    fn resolution_wasteful_does_not_fire_below_1440p() {
        // 1080p at 1 Mbps would be wasteful by bpp metric (~0.48 bps/pixel)
        // but resolution-wasteful only kicks in at ≥1440p — small
        // resolutions can legitimately be low-bitrate.
        let mut p = fine_profile();
        p.video.width = 1920;
        p.video.height = 1080;
        p.bitrate_bps = Some(1_000_000);

        let a = assess(&p, SHRINK);
        assert!(!a.issues.iter().any(|i| matches!(i, Issue::ResolutionWasteful { .. })));
    }

    #[test]
    fn shrink_mode_rules_compose_with_codec_rules() {
        // A genuinely problematic file: legacy MPEG-2 in MPEG-PS, plus an
        // excessive bitrate. Both rule families should fire.
        let mut p = fine_profile();
        p.container = Container::MpegPs;
        p.video.codec = VideoCodec::Mpeg2;
        p.bitrate_bps = Some(20_000_000); // 1080p, way above 5 Mbps threshold

        let a = assess(&p, SHRINK);
        assert!(a.issues.contains(&Issue::LegacyVideoCodec(VideoCodec::Mpeg2)));
        assert!(a.issues.contains(&Issue::LegacyContainer(Container::MpegPs)));
        assert!(a.issues.iter().any(|i| matches!(i, Issue::BitrateExcessive { .. })));
    }

    #[test]
    fn bitrate_threshold_table_matches_design() {
        // Sanity-check the table; numbers come from CLAUDE.md's "reasonable
        // bitrate" guidance.
        assert_eq!(bitrate_threshold_for_height(360), 1_500_000);
        assert_eq!(bitrate_threshold_for_height(480), 1_500_000);
        assert_eq!(bitrate_threshold_for_height(720), 3_000_000);
        assert_eq!(bitrate_threshold_for_height(1080), 5_000_000);
        assert_eq!(bitrate_threshold_for_height(1440), 9_000_000);
        assert_eq!(bitrate_threshold_for_height(2160), 15_000_000);
        assert_eq!(bitrate_threshold_for_height(4320), 15_000_000);
    }
}
