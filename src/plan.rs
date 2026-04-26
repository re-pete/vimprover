//! Step 3 of the pipeline: pure function from
//! `(profile, assessment, intent, overrides) → EncodeRecipe`.
//!
//! **Implemented in build-order step 2:** stream-copy remux to MKV for
//! legacy-container inputs. Other intents return [`Error::Unimplemented`] for
//! now.
//!
//! Planner rules (from `CLAUDE.md`) to finish in build-order steps 3–5:
//!
//! **Container choice:**
//! - MKV when preserving multichannel AC-3/DTS, or when stream-copying MPEG-2/VC-1.
//! - MP4 otherwise.
//!
//! **Video codec:** libx264 default; libx265 for >1080p or HDR.
//!
//! **CRF by resolution:** ≤576p → 20, 720p → 20, 1080p → 21, 4K → 23 (x265).
//!
//! **Filters:**
//! - Interlaced → `bwdif=1` (or `fieldmatch,decimate` when telecine heuristic fires).
//! - Non-square pixels → `scale=W:H,setsar=1`.
//! - 422/444 source + MP4 target → force `-pix_fmt yuv420p`.
//!
//! **Audio:** 5.1 → AAC stereo 192k by default; `--keep-multichannel-audio` preserves.
//!
//! **MP4 always gets** `-movflags +faststart`.

use std::path::{Path, PathBuf};

use crate::assess::Assessment;
use crate::error::{Error, Result};
use crate::model::{AudioCodec, Container, MediaProfile, PixFmt};

/// Top-level user intent. `Auto` derives the others from the assessment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Intent {
    #[default]
    Auto,
    /// Container/index repair only, no re-encode.
    Remux,
    /// Force re-encode even if codec is modern.
    Reencode,
    /// Downscale and/or re-encode smaller.
    Shrink {
        max_height: Option<u32>,
        target_bitrate_bps: Option<u64>,
    },
    /// Multi-input mode (implicit when ≥2 inputs given).
    Concat,
}

/// User overrides that bypass planner decisions.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub container: Option<Container>,
    pub video_codec: Option<VideoCodecChoice>,
    pub crf: Option<u8>,
    pub preset: Option<String>,
    pub max_height: Option<u32>,
    pub keep_multichannel_audio: bool,
}

/// User-facing video-codec selection. Distinct from [`VideoStrategy`] so the
/// CLI can express "pick x265 with default CRF and preset" without having to
/// invent values for `crf` and `preset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecChoice {
    X264,
    X265,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoStrategy {
    Copy,
    ReencodeX264 { crf: u8, preset: String },
    ReencodeX265 { crf: u8, preset: String },
    /// Single-pass average-bitrate (ABR) re-encode for shrink mode. Produces
    /// `-b:v <target> -maxrate <max> -bufsize <bufsize> -preset <preset>`.
    /// Output size lands within ~10–20% of `target_bps` for typical content.
    ReencodeX264Abr {
        target_bps: u64,
        max_bps: u64,
        bufsize_bps: u64,
        preset: String,
    },
    ReencodeX265Abr {
        target_bps: u64,
        max_bps: u64,
        bufsize_bps: u64,
        preset: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioStrategy {
    Copy,
    AacStereo { bitrate_bps: u64 },
    AacMultichannel { bitrate_bps: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoFilter {
    /// Deinterlace with `bwdif=1`.
    Bwdif,
    /// Inverse telecine: `fieldmatch,decimate`.
    FieldmatchDecimate,
    Scale { width: u32, height: u32 },
    SetSar { num: u32, den: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeRecipe {
    pub output_container: Container,
    pub video_strategy: VideoStrategy,
    pub video_filters: Vec<VideoFilter>,
    pub audio_strategy: AudioStrategy,
    pub extra_flags: Vec<String>,
}

/// Build an [`EncodeRecipe`] from a profile, assessment, and user intent.
///
/// `Intent::Auto` consults the assessment: if any issue requires a re-encode
/// (per [`Assessment::requires_reencode`]), it routes to `plan_reencode`;
/// otherwise it routes to `plan_remux` (sufficient for container-only fixes).
/// Explicit intents (`Remux`, `Reencode`) bypass the assessment and do exactly
/// what was asked.
pub fn plan(
    profile: &MediaProfile,
    assessment: &Assessment,
    intent: &Intent,
    overrides: &Overrides,
) -> Result<EncodeRecipe> {
    match intent {
        Intent::Auto => {
            if assessment.requires_reencode() {
                Ok(plan_reencode(profile, overrides))
            } else {
                Ok(plan_remux(profile, overrides))
            }
        }
        Intent::Remux => Ok(plan_remux(profile, overrides)),
        Intent::Reencode => Ok(plan_reencode(profile, overrides)),
        Intent::Shrink { max_height, target_bitrate_bps } => {
            plan_shrink(profile, overrides, *max_height, *target_bitrate_bps)
        }
        Intent::Concat => Err(Error::Unimplemented(
            "concat intent (build-order step 6)",
        )),
    }
}

fn plan_remux(_profile: &MediaProfile, overrides: &Overrides) -> EncodeRecipe {
    // When remuxing arbitrary legacy content, MKV accepts every codec vimprover
    // might encounter. The user can force MP4 via --container mp4, which may or
    // may not succeed at ffmpeg-run time depending on the source codecs.
    let output_container = overrides
        .container
        .clone()
        .unwrap_or(Container::Mkv);

    let mut extra_flags: Vec<String> = Vec::new();
    if matches!(output_container, Container::Mp4) {
        extra_flags.push("-movflags".into());
        extra_flags.push("+faststart".into());
    }

    EncodeRecipe {
        output_container,
        video_strategy: VideoStrategy::Copy,
        video_filters: Vec::new(),
        audio_strategy: AudioStrategy::Copy,
        extra_flags,
    }
}

/// Build a re-encode recipe for `Intent::Reencode`.
///
/// Pure function: every decision is derived from `profile` and `overrides`.
/// Defaults follow `CLAUDE.md`'s table; CLI overrides win when set.
fn plan_reencode(profile: &MediaProfile, overrides: &Overrides) -> EncodeRecipe {
    let output_container = overrides.container.clone().unwrap_or(Container::Mkv);
    let video_strategy = select_video_strategy(profile, overrides);
    let video_filters = select_video_filters(profile);
    let audio_strategy = select_audio_strategy(profile, overrides);
    let extra_flags = build_extra_flags(profile, &output_container);

    EncodeRecipe {
        output_container,
        video_strategy,
        video_filters,
        audio_strategy,
        extra_flags,
    }
}

/// Build a shrink recipe for `Intent::Shrink`.
///
/// Shrink mode always re-encodes (it's never a stream-copy operation) and
/// always uses single-pass ABR rate control so the output bitrate is
/// predictable. It picks `target_bps` in this order:
///
/// 1. User-supplied `target_bitrate_bps`.
/// 2. The per-resolution threshold from
///    [`crate::assess::bitrate_threshold_for_height`] applied to the
///    *output* height (which is `min(source.height, max_height)`).
///
/// Returns [`Error::NothingToShrink`] when the user gave no explicit knob
/// and the source is already at-or-below the threshold for its height —
/// re-encoding would produce a same-or-larger file, so refusing is the
/// honest answer.
fn plan_shrink(
    profile: &MediaProfile,
    overrides: &Overrides,
    max_height: Option<u32>,
    target_bitrate_bps: Option<u64>,
) -> Result<EncodeRecipe> {
    // Step 1: target output height. Never *up*scale; max_height only caps.
    let output_height = match max_height {
        Some(cap) if cap < profile.video.height => cap,
        _ => profile.video.height,
    };
    let downscaling = output_height < profile.video.height;

    // Step 2: target bitrate.
    let threshold = crate::assess::bitrate_threshold_for_height(output_height);
    let target_bps = match target_bitrate_bps {
        Some(explicit) => explicit,
        None => threshold,
    };

    // Step 3: refuse if there's literally nothing to shrink — no downscale,
    // no explicit bitrate target, and source is already at or below the
    // implicit threshold.
    if !downscaling
        && target_bitrate_bps.is_none()
        && profile.bitrate_bps.is_some_and(|src| src <= threshold)
    {
        return Err(Error::NothingToShrink(format!(
            "source is already {} at {}p (threshold for {}p is {})",
            crate::format::format_bitrate(profile.bitrate_bps.unwrap()),
            profile.video.height,
            output_height,
            crate::format::format_bitrate(threshold),
        )));
    }

    // Step 4: compose the recipe. Most pieces match plan_reencode; only the
    // video strategy and the (potential) downscale filter differ.
    let output_container = overrides.container.clone().unwrap_or(Container::Mkv);
    let preset = overrides
        .preset
        .clone()
        .unwrap_or_else(|| "medium".to_string());
    let codec = overrides
        .video_codec
        .unwrap_or_else(|| default_codec_choice(profile));

    // Single-pass ABR sized so VBV-induced peaks have headroom.
    let max_bps = target_bps + target_bps / 4; // 1.25×
    let bufsize_bps = target_bps * 2;          // 2.0×

    let video_strategy = match codec {
        VideoCodecChoice::X264 => VideoStrategy::ReencodeX264Abr {
            target_bps,
            max_bps,
            bufsize_bps,
            preset,
        },
        VideoCodecChoice::X265 => VideoStrategy::ReencodeX265Abr {
            target_bps,
            max_bps,
            bufsize_bps,
            preset,
        },
    };

    // Filters: start from the reencode set (deinterlace + square-pixel
    // correction), then prepend a downscale Scale filter when needed.
    let mut video_filters = select_video_filters(profile);
    if downscaling {
        let output_width = scaled_width_preserving_aspect(
            profile.video.width,
            profile.video.height,
            output_height,
        );
        // Prepend so the downscale runs before any post-deinterlace filters
        // (cheaper to deinterlace fewer pixels).
        video_filters.insert(0, VideoFilter::Scale {
            width: output_width,
            height: output_height,
        });
    }

    let audio_strategy = select_audio_strategy(profile, overrides);
    let extra_flags = build_extra_flags(profile, &output_container);

    Ok(EncodeRecipe {
        output_container,
        video_strategy,
        video_filters,
        audio_strategy,
        extra_flags,
    })
}

/// Compute the width that preserves the source aspect ratio when scaling to
/// `target_height`. Rounds to even (libx264 / libx265 require dimensions
/// divisible by 2 in standard 4:2:0 chroma subsampling).
fn scaled_width_preserving_aspect(
    src_width: u32,
    src_height: u32,
    target_height: u32,
) -> u32 {
    if src_height == 0 {
        return src_width; // pathological; let ffmpeg complain
    }
    let scaled = (src_width as u64 * target_height as u64) / src_height as u64;
    // Round down to nearest even number.
    (scaled & !1) as u32
}

fn select_video_strategy(profile: &MediaProfile, overrides: &Overrides) -> VideoStrategy {
    let preset = overrides
        .preset
        .clone()
        .unwrap_or_else(|| "medium".to_string());

    let codec = overrides
        .video_codec
        .unwrap_or_else(|| default_codec_choice(profile));

    let crf = overrides
        .crf
        .unwrap_or_else(|| default_crf(codec, profile.video.height));

    match codec {
        VideoCodecChoice::X264 => VideoStrategy::ReencodeX264 { crf, preset },
        VideoCodecChoice::X265 => VideoStrategy::ReencodeX265 { crf, preset },
    }
}

/// Default codec: x264 for ≤1080p SDR, x265 for >1080p or HDR.
fn default_codec_choice(profile: &MediaProfile) -> VideoCodecChoice {
    if profile.video.height > 1080 || profile.video.is_hdr {
        VideoCodecChoice::X265
    } else {
        VideoCodecChoice::X264
    }
}

/// CRF table per `CLAUDE.md`. Higher `height` → higher CRF (more compression
/// per pixel, since perceptual error tolerance grows with resolution).
fn default_crf(codec: VideoCodecChoice, height: u32) -> u8 {
    match codec {
        VideoCodecChoice::X264 => {
            if height <= 720 {
                20
            } else {
                21
            }
        }
        VideoCodecChoice::X265 => {
            if height <= 1080 {
                22
            } else {
                23
            }
        }
    }
}

fn select_video_filters(profile: &MediaProfile) -> Vec<VideoFilter> {
    let mut filters = Vec::new();

    if profile.video.field_order.is_interlaced() {
        filters.push(VideoFilter::Bwdif);
    }

    // Square-pixel correction: rescale to display size and reset SAR to 1:1.
    if let Some(sar) = profile.video.sar {
        if sar.num != sar.den {
            let (display_w, display_h) = profile.video.display_size();
            filters.push(VideoFilter::Scale {
                width: display_w,
                height: display_h,
            });
            filters.push(VideoFilter::SetSar { num: 1, den: 1 });
        }
    }

    filters
}

fn select_audio_strategy(profile: &MediaProfile, overrides: &Overrides) -> AudioStrategy {
    let Some(primary) = profile.audio.first() else {
        // No audio track: the executor's `-map 0:a?` will already drop this.
        // Returning Copy keeps the recipe small.
        return AudioStrategy::Copy;
    };

    let channels = primary.channels.unwrap_or(2);
    let is_aac = matches!(primary.codec, AudioCodec::Aac);
    let multichannel = channels > 2;

    if multichannel && overrides.keep_multichannel_audio {
        // Preserve the surround layout. Stream-copy if it's already AAC,
        // otherwise re-encode to AAC at a multichannel-appropriate bitrate.
        if is_aac {
            AudioStrategy::Copy
        } else {
            AudioStrategy::AacMultichannel {
                bitrate_bps: 384_000,
            }
        }
    } else if multichannel {
        // Default policy: downmix to AAC stereo at 192 kbps.
        AudioStrategy::AacStereo {
            bitrate_bps: 192_000,
        }
    } else if is_aac {
        // Already AAC stereo/mono — nothing to do.
        AudioStrategy::Copy
    } else {
        // Mono/stereo non-AAC (MP3, AC-3 stereo, Vorbis…) → modernize to AAC.
        AudioStrategy::AacStereo {
            bitrate_bps: 192_000,
        }
    }
}

/// Container- and source-driven extra flags applied at the tail of the
/// ffmpeg argv.
fn build_extra_flags(profile: &MediaProfile, output_container: &Container) -> Vec<String> {
    let mut extra = Vec::new();

    // Preserve JPEG-range (full-range, 0–255) colorimetry for yuvj* sources.
    // Without `-color_range pc`, ffmpeg's auto-scaler converts to limited
    // range (16–235) during transcode, crushing blacks and whites. Common on
    // screen recordings (OBS, Fraps) and JPEG-origin content.
    if is_full_range_source(profile) {
        extra.push("-color_range".into());
        extra.push("pc".into());
    }

    if matches!(output_container, Container::Mp4) {
        // Force 8-bit 4:2:0 when the source is 4:2:2 / 4:4:4 — MP4 + H.264 is
        // technically allowed for 422/444, but compatibility is poor.
        if matches!(
            profile.video.pix_fmt,
            Some(PixFmt::Yuv422p)
                | Some(PixFmt::Yuv422p10le)
                | Some(PixFmt::Yuv444p)
                | Some(PixFmt::Yuv444p10le)
                | Some(PixFmt::Yuvj422p)
                | Some(PixFmt::Yuvj444p)
        ) {
            extra.push("-pix_fmt".into());
            extra.push("yuv420p".into());
        }
        extra.push("-movflags".into());
        extra.push("+faststart".into());
    }

    extra
}

fn is_full_range_source(profile: &MediaProfile) -> bool {
    matches!(
        profile.video.pix_fmt,
        Some(PixFmt::Yuvj420p) | Some(PixFmt::Yuvj422p) | Some(PixFmt::Yuvj444p)
    )
}

/// Turn the user-supplied output name into a concrete path, using the
/// recipe's container extension when the user didn't supply a recognized one.
///
/// `Path::extension` returns the substring after the *last* `.` in the file
/// name, so a name like `Tutorial #10 (MC 1.7.10) (Low)` looks like it has the
/// extension `10) (Low)`. We only honor extensions on a small allow-list of
/// output-supportable containers; anything else (including no extension at
/// all) gets the canonical extension *appended* — never substituted via
/// `Path::with_extension`, which would mangle the filename.
pub fn resolve_output_path(user_output: &Path, recipe: &EncodeRecipe) -> PathBuf {
    if let Some(ext) = user_output.extension().and_then(|e| e.to_str()) {
        if is_known_output_extension(ext) {
            return user_output.to_path_buf();
        }
    }

    let mut buf = user_output.as_os_str().to_os_string();
    buf.push(".");
    buf.push(recipe.output_container.canonical_extension());
    PathBuf::from(buf)
}

/// Returns true if `ext` is one of the container extensions vimprover knows
/// how to write (case-insensitive, leading `.` not expected).
pub fn is_known_output_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mkv" | "mp4" | "m4v" | "mov" | "webm"
    )
}

/// Best-effort inference of the desired output [`Container`] from the user's
/// output-path extension. Returns `None` for paths with no extension or with
/// an unrecognized one. Intended for the CLI to wire `out.mp4` → MP4 etc.
/// without requiring an explicit `--container` flag.
pub fn container_from_output_extension(path: &Path) -> Option<Container> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    match ext.to_ascii_lowercase().as_str() {
        "mkv" => Some(Container::Mkv),
        "mp4" | "m4v" => Some(Container::Mp4),
        // mov/webm intentionally not yet wired into Container enum routing.
        _ => None,
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

    fn sample_profile() -> MediaProfile {
        MediaProfile {
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
            duration_secs: Some(60.0),
            file_size_bytes: Some(10_000_000),
            bitrate_bps: Some(1_000_000),
        }
    }

    #[test]
    fn auto_intent_with_empty_assessment_plans_mkv_remux() {
        // Default-empty Assessment ⇒ requires_reencode() is false ⇒ remux.
        // (`sample_profile()` itself has issues, but the caller didn't run
        // `assess()`; we honor what they passed.)
        let p = sample_profile();
        let recipe = plan(&p, &Assessment::default(), &Intent::Auto, &Overrides::default())
            .expect("plan succeeds for Auto");
        assert_eq!(recipe.output_container, Container::Mkv);
        assert_eq!(recipe.video_strategy, VideoStrategy::Copy);
        assert_eq!(recipe.audio_strategy, AudioStrategy::Copy);
        assert!(recipe.video_filters.is_empty());
        assert!(recipe.extra_flags.is_empty());
    }

    #[test]
    fn auto_intent_with_remux_only_issue_picks_remux() {
        let p = sample_profile();
        let assessment = Assessment {
            issues: vec![crate::assess::Issue::LegacyContainer(Container::MpegPs)],
        };
        let recipe = plan(&p, &assessment, &Intent::Auto, &Overrides::default())
            .expect("plan succeeds for Auto");
        assert_eq!(recipe.video_strategy, VideoStrategy::Copy);
    }

    #[test]
    fn auto_intent_with_reencode_required_issue_picks_reencode() {
        let p = sample_profile();
        let assessment = Assessment {
            issues: vec![crate::assess::Issue::Interlaced],
        };
        let recipe = plan(&p, &assessment, &Intent::Auto, &Overrides::default())
            .expect("plan succeeds for Auto");
        assert!(matches!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX264 { .. }
        ));
        // The interlaced sample profile should also pick up the bwdif filter.
        assert!(recipe.video_filters.contains(&VideoFilter::Bwdif));
    }

    // -- Intent::Shrink ------------------------------------------------------

    /// 1080p H.264/AAC MKV at 12 Mbps — well above the 5 Mbps threshold,
    /// so an unconfigured shrink should target the threshold.
    fn oversized_1080p_profile() -> MediaProfile {
        MediaProfile {
            container: Container::Mkv,
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
            file_size_bytes: Some(90_000_000),
            bitrate_bps: Some(12_000_000),
        }
    }

    #[test]
    fn shrink_default_targets_threshold_for_source_height() {
        let p = oversized_1080p_profile();
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: None,
                target_bitrate_bps: None,
            },
            &Overrides::default(),
        )
        .expect("plan_shrink succeeds for oversized 1080p");

        // ABR at the 1080p threshold (5 Mbps), max = 1.25× target,
        // bufsize = 2× target.
        match &recipe.video_strategy {
            VideoStrategy::ReencodeX264Abr {
                target_bps,
                max_bps,
                bufsize_bps,
                preset,
            } => {
                assert_eq!(*target_bps, 5_000_000);
                assert_eq!(*max_bps, 6_250_000);
                assert_eq!(*bufsize_bps, 10_000_000);
                assert_eq!(preset, "medium");
            }
            other => panic!("expected ReencodeX264Abr, got {other:?}"),
        }
        // No downscale → no Scale filter.
        assert!(!recipe.video_filters.iter().any(|f| matches!(f, VideoFilter::Scale { .. })));
    }

    #[test]
    fn shrink_with_max_height_downscales_and_targets_smaller_threshold() {
        let p = oversized_1080p_profile();
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: Some(720),
                target_bitrate_bps: None,
            },
            &Overrides::default(),
        )
        .expect("plan_shrink");

        // Target should now be the 720p threshold (3 Mbps), not 1080p's.
        match &recipe.video_strategy {
            VideoStrategy::ReencodeX264Abr { target_bps, .. } => {
                assert_eq!(*target_bps, 3_000_000);
            }
            other => panic!("expected ReencodeX264Abr, got {other:?}"),
        }

        // Scale filter prepended with width preserving 16:9 aspect:
        // 1920 × 720 / 1080 = 1280.
        let scale = recipe
            .video_filters
            .iter()
            .find(|f| matches!(f, VideoFilter::Scale { .. }))
            .expect("expected Scale filter");
        assert_eq!(*scale, VideoFilter::Scale { width: 1280, height: 720 });
    }

    #[test]
    fn shrink_with_explicit_target_bitrate_uses_it_verbatim() {
        let p = oversized_1080p_profile();
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: None,
                target_bitrate_bps: Some(2_500_000), // user wants ~2.5 Mbps
            },
            &Overrides::default(),
        )
        .expect("plan_shrink");

        match &recipe.video_strategy {
            VideoStrategy::ReencodeX264Abr {
                target_bps,
                max_bps,
                bufsize_bps,
                ..
            } => {
                assert_eq!(*target_bps, 2_500_000);
                assert_eq!(*max_bps, 3_125_000);
                assert_eq!(*bufsize_bps, 5_000_000);
            }
            other => panic!("expected ReencodeX264Abr, got {other:?}"),
        }
    }

    #[test]
    fn shrink_refuses_when_source_already_at_or_below_threshold() {
        let mut p = oversized_1080p_profile();
        p.bitrate_bps = Some(4_000_000); // below the 5 Mbps threshold

        let result = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: None,
                target_bitrate_bps: None,
            },
            &Overrides::default(),
        );

        match result {
            Err(Error::NothingToShrink(msg)) => {
                assert!(
                    msg.contains("1080p"),
                    "error message should mention source resolution: {msg}"
                );
            }
            other => panic!("expected NothingToShrink, got {other:?}"),
        }
    }

    #[test]
    fn shrink_with_explicit_target_proceeds_even_if_source_is_small() {
        // User explicitly wants a smaller target — we trust them even if
        // the source is already under the implicit threshold.
        let mut p = oversized_1080p_profile();
        p.bitrate_bps = Some(4_000_000);

        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: None,
                target_bitrate_bps: Some(1_000_000),
            },
            &Overrides::default(),
        )
        .expect("explicit target overrides nothing-to-shrink");

        match &recipe.video_strategy {
            VideoStrategy::ReencodeX264Abr { target_bps, .. } => {
                assert_eq!(*target_bps, 1_000_000);
            }
            other => panic!("expected ReencodeX264Abr, got {other:?}"),
        }
    }

    #[test]
    fn shrink_with_max_height_above_source_doesnt_upscale() {
        // Source is 1080p, user passes --max-height 1440. We should NOT
        // upscale; we should keep 1080p.
        let p = oversized_1080p_profile();
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: Some(1440),
                target_bitrate_bps: None,
            },
            &Overrides::default(),
        )
        .expect("plan_shrink");

        // No Scale filter (no resolution change).
        assert!(!recipe.video_filters.iter().any(|f| matches!(f, VideoFilter::Scale { .. })));
        // Bitrate target stays at 1080p's threshold.
        match &recipe.video_strategy {
            VideoStrategy::ReencodeX264Abr { target_bps, .. } => {
                assert_eq!(*target_bps, 5_000_000);
            }
            other => panic!("expected ReencodeX264Abr, got {other:?}"),
        }
    }

    #[test]
    fn shrink_picks_x265_for_4k_or_hdr_sources() {
        // 4K source: default codec is x265. Shrink should respect that.
        let mut p = oversized_1080p_profile();
        p.video.width = 3840;
        p.video.height = 2160;
        p.bitrate_bps = Some(40_000_000); // 4K @ 40 Mbps; threshold is 15 Mbps

        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: None,
                target_bitrate_bps: None,
            },
            &Overrides::default(),
        )
        .expect("plan_shrink");

        assert!(matches!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX265Abr { .. }
        ));
    }

    #[test]
    fn scaled_width_preserving_aspect_rounds_to_even() {
        // Standard 16:9 cases all give even widths.
        assert_eq!(scaled_width_preserving_aspect(1920, 1080, 720), 1280);
        assert_eq!(scaled_width_preserving_aspect(3840, 2160, 1080), 1920);
        assert_eq!(scaled_width_preserving_aspect(1920, 1080, 480), 852);

        // Non-standard aspect that would compute to an odd width:
        // 1919 × 720 / 1080 = 1279.33 → 1279 → rounded down to 1278.
        assert_eq!(scaled_width_preserving_aspect(1919, 1080, 720), 1278);
    }

    #[test]
    fn remux_intent_honors_mp4_override_and_adds_faststart() {
        let p = sample_profile();
        let overrides = Overrides {
            container: Some(Container::Mp4),
            ..Overrides::default()
        };
        let recipe =
            plan(&p, &Assessment::default(), &Intent::Remux, &overrides).expect("plan");
        assert_eq!(recipe.output_container, Container::Mp4);
        assert_eq!(
            recipe.extra_flags,
            vec!["-movflags".to_string(), "+faststart".to_string()]
        );
    }

    fn progressive_profile(
        codec: VideoCodec,
        height: u32,
        is_hdr: bool,
        audio: AudioInfo,
        pix_fmt: Option<PixFmt>,
    ) -> MediaProfile {
        let width = match height {
            480 => 854,
            720 => 1280,
            1080 => 1920,
            2160 => 3840,
            _ => height * 16 / 9,
        };
        MediaProfile {
            container: Container::Mkv,
            video: VideoInfo {
                codec,
                width,
                height,
                field_order: FieldOrder::Progressive,
                framerate: Rational::new(24, 1),
                pix_fmt,
                sar: Rational::new(1, 1),
                is_hdr,
            },
            audio: vec![audio],
            duration_secs: Some(60.0),
            file_size_bytes: Some(1_000_000),
            bitrate_bps: Some(500_000),
        }
    }

    fn aac_stereo() -> AudioInfo {
        AudioInfo {
            codec: AudioCodec::Aac,
            channels: Some(2),
            channel_layout: Some("stereo".into()),
            sample_rate_hz: Some(48_000),
            bitrate_bps: Some(128_000),
            language: None,
        }
    }

    fn mp3_stereo() -> AudioInfo {
        AudioInfo {
            codec: AudioCodec::Mp3,
            channels: Some(2),
            channel_layout: Some("stereo".into()),
            sample_rate_hz: Some(44_100),
            bitrate_bps: Some(128_000),
            language: None,
        }
    }

    #[test]
    fn reencode_picks_x264_for_1080p_sdr() {
        let p = progressive_profile(
            VideoCodec::H264,
            1080,
            false,
            aac_stereo(),
            Some(PixFmt::Yuv420p),
        );
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Reencode,
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX264 { crf: 21, preset: "medium".into() }
        );
        // AAC stereo should stream-copy.
        assert_eq!(recipe.audio_strategy, AudioStrategy::Copy);
        // No filters needed for square-pixel progressive content.
        assert!(recipe.video_filters.is_empty());
        // Default container is MKV; no extra flags.
        assert_eq!(recipe.output_container, Container::Mkv);
        assert!(recipe.extra_flags.is_empty());
    }

    #[test]
    fn reencode_picks_x264_crf20_at_720p() {
        let p = progressive_profile(
            VideoCodec::H264,
            720,
            false,
            aac_stereo(),
            Some(PixFmt::Yuv420p),
        );
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Reencode,
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX264 { crf: 20, preset: "medium".into() }
        );
    }

    #[test]
    fn reencode_picks_x265_for_4k() {
        let p = progressive_profile(
            VideoCodec::H265,
            2160,
            false,
            aac_stereo(),
            Some(PixFmt::Yuv420p10le),
        );
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Reencode,
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX265 { crf: 23, preset: "medium".into() }
        );
    }

    #[test]
    fn reencode_picks_x265_for_hdr_at_1080p() {
        let p = progressive_profile(
            VideoCodec::H265,
            1080,
            true,
            aac_stereo(),
            Some(PixFmt::Yuv420p10le),
        );
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Reencode,
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX265 { crf: 22, preset: "medium".into() }
        );
    }

    #[test]
    fn reencode_deinterlaces_interlaced_source() {
        // sample_profile() is the DVD-shaped interlaced MPEG-2 input.
        let p = sample_profile();
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Reencode,
            &Overrides::default(),
        )
        .unwrap();
        assert!(recipe.video_filters.contains(&VideoFilter::Bwdif));
        // Non-square pixels (10:11 SAR) → scale to display size + setsar 1:1.
        assert!(matches!(
            recipe.video_filters.iter().find(|f| matches!(f, VideoFilter::Scale { .. })),
            Some(VideoFilter::Scale { .. })
        ));
        assert!(recipe
            .video_filters
            .contains(&VideoFilter::SetSar { num: 1, den: 1 }));
        // 5.1 AC-3 → AAC stereo by default.
        assert_eq!(
            recipe.audio_strategy,
            AudioStrategy::AacStereo { bitrate_bps: 192_000 }
        );
    }

    #[test]
    fn reencode_preserves_multichannel_when_requested() {
        let p = sample_profile();
        let overrides = Overrides {
            keep_multichannel_audio: true,
            ..Overrides::default()
        };
        let recipe =
            plan(&p, &Assessment::default(), &Intent::Reencode, &overrides).unwrap();
        // Source is AC-3 → re-encode to multichannel AAC.
        assert_eq!(
            recipe.audio_strategy,
            AudioStrategy::AacMultichannel { bitrate_bps: 384_000 }
        );
    }

    #[test]
    fn reencode_modernizes_mp3_stereo_to_aac() {
        // Hot-Fuzz-shaped profile: MPEG-4 Part 2 video + MP3 stereo.
        let p = progressive_profile(
            VideoCodec::Mpeg4,
            480,
            false,
            mp3_stereo(),
            Some(PixFmt::Yuv420p),
        );
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Reencode,
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(
            recipe.audio_strategy,
            AudioStrategy::AacStereo { bitrate_bps: 192_000 }
        );
    }

    #[test]
    fn reencode_honors_overrides() {
        let p = progressive_profile(
            VideoCodec::H264,
            1080,
            false,
            aac_stereo(),
            Some(PixFmt::Yuv420p),
        );
        let overrides = Overrides {
            video_codec: Some(VideoCodecChoice::X265),
            crf: Some(28),
            preset: Some("slow".into()),
            ..Overrides::default()
        };
        let recipe =
            plan(&p, &Assessment::default(), &Intent::Reencode, &overrides).unwrap();
        assert_eq!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX265 { crf: 28, preset: "slow".into() }
        );
    }

    #[test]
    fn reencode_mp4_container_adds_faststart_and_pixfmt_for_yuv422() {
        let p = progressive_profile(
            VideoCodec::H264,
            1080,
            false,
            aac_stereo(),
            Some(PixFmt::Yuv422p),
        );
        let overrides = Overrides {
            container: Some(Container::Mp4),
            ..Overrides::default()
        };
        let recipe =
            plan(&p, &Assessment::default(), &Intent::Reencode, &overrides).unwrap();
        assert_eq!(recipe.output_container, Container::Mp4);
        assert_eq!(
            recipe.extra_flags,
            vec![
                "-pix_fmt".to_string(),
                "yuv420p".to_string(),
                "-movflags".to_string(),
                "+faststart".to_string(),
            ]
        );
    }

    #[test]
    fn reencode_preserves_full_range_for_yuvj420_source() {
        let p = progressive_profile(
            VideoCodec::H264,
            720,
            false,
            aac_stereo(),
            Some(PixFmt::Yuvj420p),
        );
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Reencode,
            &Overrides::default(),
        )
        .unwrap();
        // MKV default: only the color-range flag should appear, not -pix_fmt
        // (yuvj420p is already 4:2:0) and not +faststart (we're not in MP4).
        assert_eq!(
            recipe.extra_flags,
            vec!["-color_range".to_string(), "pc".to_string()]
        );
    }

    #[test]
    fn reencode_yuvj422_mp4_emits_color_range_pixfmt_and_faststart() {
        let p = progressive_profile(
            VideoCodec::H264,
            1080,
            false,
            aac_stereo(),
            Some(PixFmt::Yuvj422p),
        );
        let overrides = Overrides {
            container: Some(Container::Mp4),
            ..Overrides::default()
        };
        let recipe =
            plan(&p, &Assessment::default(), &Intent::Reencode, &overrides).unwrap();
        // All three guards should fire in order: full-range tag, pixfmt
        // downconvert, MP4 faststart.
        assert_eq!(
            recipe.extra_flags,
            vec![
                "-color_range".to_string(),
                "pc".to_string(),
                "-pix_fmt".to_string(),
                "yuv420p".to_string(),
                "-movflags".to_string(),
                "+faststart".to_string(),
            ]
        );
    }

    #[test]
    fn reencode_yuv420_source_does_not_emit_color_range() {
        let p = progressive_profile(
            VideoCodec::H264,
            720,
            false,
            aac_stereo(),
            Some(PixFmt::Yuv420p),
        );
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Reencode,
            &Overrides::default(),
        )
        .unwrap();
        assert!(
            recipe.extra_flags.is_empty(),
            "yuv420p source should not trigger color-range flag: {:?}",
            recipe.extra_flags
        );
    }

    #[test]
    fn reencode_mp4_container_skips_pixfmt_for_yuv420() {
        let p = progressive_profile(
            VideoCodec::H264,
            1080,
            false,
            aac_stereo(),
            Some(PixFmt::Yuv420p),
        );
        let overrides = Overrides {
            container: Some(Container::Mp4),
            ..Overrides::default()
        };
        let recipe =
            plan(&p, &Assessment::default(), &Intent::Reencode, &overrides).unwrap();
        assert_eq!(
            recipe.extra_flags,
            vec!["-movflags".to_string(), "+faststart".to_string()]
        );
    }

    #[test]
    fn concat_intent_is_unimplemented() {
        let p = sample_profile();
        let err = plan(
            &p,
            &Assessment::default(),
            &Intent::Concat,
            &Overrides::default(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Unimplemented(_)));
    }

    #[test]
    fn resolve_output_path_appends_extension_when_missing() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let out = resolve_output_path(Path::new("newname"), &recipe);
        assert_eq!(out, PathBuf::from("newname.mkv"));
    }

    #[test]
    fn resolve_output_path_preserves_recognized_extension() {
        let overrides = Overrides {
            container: Some(Container::Mp4),
            ..Overrides::default()
        };
        let recipe = plan_remux(&sample_profile(), &overrides);
        let out = resolve_output_path(Path::new("newname.mp4"), &recipe);
        assert_eq!(out, PathBuf::from("newname.mp4"));
    }

    #[test]
    fn resolve_output_path_recognizes_extension_case_insensitively() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let out = resolve_output_path(Path::new("newname.MKV"), &recipe);
        assert_eq!(out, PathBuf::from("newname.MKV"));
    }

    #[test]
    fn resolve_output_path_handles_paths_with_directories() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let out = resolve_output_path(Path::new("/tmp/subdir/movie"), &recipe);
        assert_eq!(out, PathBuf::from("/tmp/subdir/movie.mkv"));
    }

    /// Regression: `Path::extension` returns `"10) (Low)"` for the user's
    /// Applied-Energistics tutorial filename, fooling the old code into
    /// skipping the `.mkv` append. ffmpeg then can't infer the format and
    /// errors out with `Unable to choose an output format`.
    #[test]
    fn resolve_output_path_appends_when_filename_has_embedded_periods() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let user = Path::new(
            "out/Applied Energistics 2 Tutorial #10 Auto-Crafting  \
             Crafting Networks (MC 1.7.10) (Low)",
        );
        let out = resolve_output_path(user, &recipe);
        assert_eq!(
            out,
            PathBuf::from(
                "out/Applied Energistics 2 Tutorial #10 Auto-Crafting  \
                 Crafting Networks (MC 1.7.10) (Low).mkv"
            )
        );
    }

    #[test]
    fn resolve_output_path_appends_when_extension_is_unknown() {
        // `.flv` is an input format vimprover handles, but not an output
        // container we can write — fall through to the recipe's extension.
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let out = resolve_output_path(Path::new("newname.flv"), &recipe);
        assert_eq!(out, PathBuf::from("newname.flv.mkv"));
    }

    #[test]
    fn container_from_output_extension_recognizes_common_outputs() {
        assert_eq!(
            container_from_output_extension(Path::new("foo.mkv")),
            Some(Container::Mkv)
        );
        assert_eq!(
            container_from_output_extension(Path::new("foo.mp4")),
            Some(Container::Mp4)
        );
        assert_eq!(
            container_from_output_extension(Path::new("foo.M4V")),
            Some(Container::Mp4)
        );
        assert_eq!(container_from_output_extension(Path::new("foo")), None);
        assert_eq!(
            container_from_output_extension(Path::new("foo.flv")),
            None
        );
        // Embedded-period filename: "extension" is gibberish, no inference.
        assert_eq!(
            container_from_output_extension(Path::new("Tutorial (MC 1.7.10) (Low)")),
            None
        );
    }
}
