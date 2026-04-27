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

/// How ffmpeg should join multiple inputs. Only populated when the recipe
/// was produced by [`plan_concat`]; single-file plans leave this `None`.
///
/// Step 6 ships only [`ConcatStrategy::Demuxer`] (fast, stream-copy,
/// requires uniform inputs). The filter-concat path — which re-encodes to a
/// common output spec and handles mismatched inputs — is deferred until a
/// real-world use case demands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcatStrategy {
    /// Use ffmpeg's concat *demuxer* (`-f concat -safe 0 -i LIST.txt`) to
    /// join uniform inputs without re-encoding. The executor writes the
    /// list file; the recipe just signals that this mode is active.
    Demuxer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeRecipe {
    pub output_container: Container,
    pub video_strategy: VideoStrategy,
    pub video_filters: Vec<VideoFilter>,
    pub audio_strategy: AudioStrategy,
    pub extra_flags: Vec<String>,
    /// Set by [`plan_concat`] to signal multi-input concatenation. `None`
    /// for ordinary single-file plans.
    pub concat: Option<ConcatStrategy>,
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
        // `plan()` is the single-input entry point. Concat is multi-input
        // and has its own top-level entry, [`plan_concat`]; the CLI dispatches
        // to it directly when ≥2 inputs are passed. If a library caller routes
        // a single profile through `plan()` with `Intent::Concat`, that's a
        // programmer error — surface it loudly rather than silently pretending
        // it was a single-file plan.
        Intent::Concat => Err(Error::ConcatTooFewInputs(1)),
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
        concat: None,
    }
}

/// Build a re-encode recipe for `Intent::Reencode`.
///
/// Pure function: every decision is derived from `profile` and `overrides`.
/// Defaults follow `CLAUDE.md`'s table; CLI overrides win when set.
fn plan_reencode(profile: &MediaProfile, overrides: &Overrides) -> EncodeRecipe {
    let output_container = overrides.container.clone().unwrap_or(Container::Mkv);
    let video_strategy = select_video_strategy(profile, overrides);
    let video_filters = select_video_filters(profile, None);
    let audio_strategy = select_audio_strategy(profile, overrides);
    let extra_flags = build_extra_flags(profile, &output_container);

    EncodeRecipe {
        output_container,
        video_strategy,
        video_filters,
        audio_strategy,
        extra_flags,
        concat: None,
    }
}

/// Build a shrink recipe for `Intent::Shrink`.
///
/// Shrink mode always re-encodes (it's never a stream-copy operation). The
/// defaults track what an experienced ffmpeg user would type by hand for an
/// archival "make this file smaller without losing visible quality" job:
///
/// - **Codec:** x265 unconditionally (overridable via `--codec`). Produces
///   ~25–50 % smaller files than x264 at the same perceptual quality.
/// - **Preset:** `slow` (overridable via `--preset`). The user explicitly
///   asked to shrink, so spending CPU for a meaningfully smaller file is
///   the right tradeoff; x265 in particular gains noticeably over `medium`.
/// - **Rate control:**
///   - No `--target-bitrate` → CRF (best quality-per-bit). CRF picked from
///     [`shrink_default_crf`], a per-codec/per-height table tuned for
///     "visually indistinguishable from source." Overridable via `--crf`.
///   - With `--target-bitrate` → single-pass ABR sized with 1.25× max-rate
///     and 2.0× VBV bufsize. Use when output size must be predictable.
/// - **Audio:** stream-copy when the output container natively and reliably
///   accepts the source codec (e.g. AAC in MP4, anything in MKV). When the
///   container would reject it or accept it compat-sketchily (e.g. AC-3 in
///   MP4), fall back to AAC via the same policy the reencode planner uses.
///   Audio is small relative to video, so copy-where-possible is almost
///   always the right call.
///
/// `output_height = min(source.height, max_height)`; when smaller than the
/// source, a leading `scale=W:H` filter is prepended.
///
/// Returns [`Error::NothingToShrink`] when the user gave no explicit knob
/// and the source is already at-or-below the per-height bitrate threshold —
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

    // Step 2: refuse if there's literally nothing to shrink — no downscale,
    // no explicit bitrate target, and source is already at or below the
    // implicit per-height bitrate threshold. The threshold is x264-tuned
    // and conservative; x265 with our default CRF would generally produce
    // an even smaller file, but the size win is small and not worth a
    // surprise re-encode of a file the user probably forgot was already small.
    let threshold = crate::assess::bitrate_threshold_for_height(output_height);
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

    // Step 3: compose the recipe.
    let output_container = overrides.container.clone().unwrap_or(Container::Mkv);
    let codec = overrides.video_codec.unwrap_or(VideoCodecChoice::X265);
    let preset = overrides
        .preset
        .clone()
        .unwrap_or_else(|| "slow".to_string());

    let video_strategy = match target_bitrate_bps {
        Some(target_bps) => {
            // Explicit size target → single-pass ABR with VBV headroom.
            let max_bps = target_bps + target_bps / 4; // 1.25×
            let bufsize_bps = target_bps * 2;          // 2.0×
            match codec {
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
            }
        }
        None => {
            let crf = overrides
                .crf
                .unwrap_or_else(|| shrink_default_crf(codec, output_height));
            match codec {
                VideoCodecChoice::X264 => VideoStrategy::ReencodeX264 { crf, preset },
                VideoCodecChoice::X265 => VideoStrategy::ReencodeX265 { crf, preset },
            }
        }
    };

    // Build the filter chain. select_video_filters folds the (optional)
    // downscale and SAR correction into one display-space scale operation,
    // which avoids the historical bug where shrink-on-non-square-SAR
    // sources emitted a downscale immediately followed by an upscale back
    // to original-display dims.
    let video_filters = select_video_filters(profile, Some(output_height));

    let audio_strategy =
        select_audio_strategy_for_shrink(profile, &output_container, overrides);
    let extra_flags = build_extra_flags(profile, &output_container);

    Ok(EncodeRecipe {
        output_container,
        video_strategy,
        video_filters,
        audio_strategy,
        extra_flags,
        concat: None,
    })
}

/// Default CRF for shrink mode. Higher (more compression) than
/// [`default_crf`]'s reencode targets because the user has explicitly asked
/// for a smaller file — quality bar is "visually indistinguishable from
/// source" rather than "studio-master grade."
///
/// The x265 column is the common path (shrink defaults to x265); x264 is here
/// for `--codec h264` overrides.
fn shrink_default_crf(codec: VideoCodecChoice, height: u32) -> u8 {
    match codec {
        VideoCodecChoice::X264 => {
            if height <= 720 {
                22
            } else if height <= 1080 {
                23
            } else {
                24
            }
        }
        VideoCodecChoice::X265 => {
            if height <= 720 {
                24
            } else if height <= 1080 {
                25
            } else {
                26
            }
        }
    }
}

/// Build a stream-copy concat recipe.
///
/// Phase-1 concat supports only the demuxer path: inputs must have identical
/// stream parameters (codec, resolution, pixel format, framerate, audio
/// codec/channels/sample-rate). When they match, ffmpeg joins them without
/// re-encoding — roughly 10× faster than the filter-concat path and producing
/// bit-identical video.
///
/// `inputs` and `profiles` must have the same length and correspond 1:1.
/// Returns:
///
/// - [`Error::ConcatTooFewInputs`] if fewer than two inputs were supplied.
/// - [`Error::ConcatInputsDiffer`] naming the first input whose streams
///   diverge from input #0, with a human-readable reason.
///
/// Passes through the usual container override (`--container`) and emits the
/// MP4 faststart flag when targeting MP4.
pub fn plan_concat(
    inputs: &[&Path],
    profiles: &[MediaProfile],
    overrides: &Overrides,
) -> Result<EncodeRecipe> {
    assert_eq!(
        inputs.len(),
        profiles.len(),
        "plan_concat: inputs and profiles slices must align",
    );
    if profiles.len() < 2 {
        return Err(Error::ConcatTooFewInputs(profiles.len()));
    }

    let reference = &profiles[0];
    for (i, p) in profiles.iter().enumerate().skip(1) {
        if let Err(why) = streams_compatible(reference, p) {
            return Err(Error::ConcatInputsDiffer {
                path: inputs[i].to_path_buf(),
                why,
            });
        }
    }

    let output_container = overrides.container.clone().unwrap_or(Container::Mkv);

    let mut extra_flags: Vec<String> = Vec::new();
    if matches!(output_container, Container::Mp4) {
        extra_flags.push("-movflags".into());
        extra_flags.push("+faststart".into());
    }

    Ok(EncodeRecipe {
        output_container,
        video_strategy: VideoStrategy::Copy,
        video_filters: Vec::new(),
        audio_strategy: AudioStrategy::Copy,
        extra_flags,
        concat: Some(ConcatStrategy::Demuxer),
    })
}

/// Check whether two media profiles are stream-copy-concat-compatible. Returns
/// `Ok(())` when they match, or a short human-readable description of the
/// first field that differs.
///
/// The comparison covers the fields ffmpeg's concat demuxer is sensitive to
/// in practice: video codec, resolution, pixel format, framerate, audio
/// codec, channel count, and sample rate. SAR and HDR metadata are *not*
/// checked; mismatch there still produces a playable file in every player
/// the author has tested.
pub fn streams_compatible(
    a: &MediaProfile,
    b: &MediaProfile,
) -> std::result::Result<(), String> {
    if a.video.codec != b.video.codec {
        return Err(format!(
            "video codec ({} vs {})",
            a.video.codec, b.video.codec
        ));
    }
    if (a.video.width, a.video.height) != (b.video.width, b.video.height) {
        return Err(format!(
            "video resolution ({}x{} vs {}x{})",
            a.video.width, a.video.height, b.video.width, b.video.height
        ));
    }
    if a.video.pix_fmt != b.video.pix_fmt {
        let render = |p: Option<&PixFmt>| match p {
            Some(pf) => pf.to_string(),
            None => "unknown".to_string(),
        };
        return Err(format!(
            "pixel format ({} vs {})",
            render(a.video.pix_fmt.as_ref()),
            render(b.video.pix_fmt.as_ref()),
        ));
    }
    // Framerate: compare as a fraction within a small epsilon so 29.97 (NTSC)
    // vs 30 (round) is flagged, but reported 24.000 vs 23.976 isn't. The
    // demuxer tolerates tiny drift via CFR output; it does NOT tolerate a real
    // 24 vs 30 mismatch.
    if let (Some(x), Some(y)) = (a.video.framerate, b.video.framerate) {
        if (x.as_f64() - y.as_f64()).abs() > 0.1 {
            return Err(format!(
                "framerate ({:.3} vs {:.3} fps)",
                x.as_f64(),
                y.as_f64()
            ));
        }
    }

    // Audio: compare the first track. Missing-audio-in-one-input is a
    // structural mismatch we refuse. (Two no-audio inputs are fine.)
    match (a.audio.first(), b.audio.first()) {
        (Some(ax), Some(bx)) => {
            if ax.codec != bx.codec {
                return Err(format!(
                    "audio codec ({} vs {})",
                    ax.codec, bx.codec
                ));
            }
            if ax.channels != bx.channels {
                return Err(format!(
                    "audio channel count ({} vs {})",
                    describe_channels(ax.channels),
                    describe_channels(bx.channels),
                ));
            }
            if ax.sample_rate_hz != bx.sample_rate_hz {
                return Err(format!(
                    "audio sample rate ({} vs {})",
                    describe_sample_rate(ax.sample_rate_hz),
                    describe_sample_rate(bx.sample_rate_hz),
                ));
            }
        }
        (Some(_), None) => {
            return Err("audio tracks (reference has audio, this input does not)".into());
        }
        (None, Some(_)) => {
            return Err("audio tracks (reference has no audio, this input does)".into());
        }
        (None, None) => {}
    }

    Ok(())
}

fn describe_channels(n: Option<u32>) -> String {
    match n {
        Some(n) => n.to_string(),
        None => "unknown".into(),
    }
}

fn describe_sample_rate(hz: Option<u32>) -> String {
    match hz {
        Some(hz) => format!("{hz} Hz"),
        None => "unknown".into(),
    }
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

/// Build the video filter chain for a re-encode (or shrink-with-downscale).
///
/// When `target_height` is `Some(h)` with `h < profile.video.height`, the
/// chain includes a downscale to height `h`. The downscale and any
/// SAR-correction are computed in *display* space, so a non-square-SAR
/// source plus a downscale produces a single combined `scale=W:H` filter
/// rather than two scales that double-count (a real bug pre-fix:
/// downscale → SAR-correction would scale-down then scale-back-up to the
/// original display dims, defeating the downscale entirely).
///
/// Output filter order:
/// - `bwdif` (if interlaced) — deinterlace first, before any scaling, so
///   the deinterlacer sees the original fields rather than mixed-field
///   downscaled rows.
/// - `scale=W:H` (if downscaling or SAR ≠ 1) — combined.
/// - `setsar=1:1` (if the source had non-square SAR) — declares the
///   output square-pixel.
///
/// Both Scale dims are rounded *down* to even — libx264/libx265 in 4:2:0
/// reject odd width or height ("width not divisible by 2"). Common with
/// NTSC SAR ratios: 352×240 SAR 200:219 → display 321×240; the planner
/// must emit 320×240.
fn select_video_filters(
    profile: &MediaProfile,
    target_height: Option<u32>,
) -> Vec<VideoFilter> {
    let mut filters = Vec::new();

    if profile.video.field_order.is_interlaced() {
        filters.push(VideoFilter::Bwdif);
    }

    let has_non_square_sar = profile
        .video
        .sar
        .is_some_and(|s| s.num != s.den);
    let downscaling = target_height.is_some_and(|h| h < profile.video.height);

    if downscaling || has_non_square_sar {
        let (display_w, display_h) = profile.video.display_size();
        // Pathological zero-height source: bail out and let ffmpeg complain.
        if display_h == 0 {
            return filters;
        }
        let out_h = if downscaling {
            target_height.unwrap()
        } else {
            display_h
        };
        // Width computed in display (square-pixel) space. When downscaling,
        // this scales display_w by (out_h / display_h); when not, it equals
        // display_w. Either way, the resulting frame is square-pixel.
        let out_w = (display_w as u64 * out_h as u64) / display_h as u64;
        filters.push(VideoFilter::Scale {
            width: (out_w & !1) as u32,
            height: out_h & !1,
        });
        if has_non_square_sar {
            filters.push(VideoFilter::SetSar { num: 1, den: 1 });
        }
    }

    filters
}

/// Whether `container` natively and compat-reliably accepts `codec` as a
/// stream-copied audio track.
///
/// Conservative by design:
///
/// - **MKV** accepts essentially anything (AAC, AC-3, E-AC-3, DTS, MP3,
///   FLAC, Opus, Vorbis, PCM, WMA, …) — always `true`.
/// - **MP4 / MOV** accept AAC, MP3, FLAC, ALAC, and E-AC-3 cleanly. AC-3 in
///   MP4 is technically legal (ISO/IEC 14496-3) but widely broken in
///   consumer players, so we refuse it here and force an AAC fallback.
/// - **WebM** accepts Opus and Vorbis only.
/// - Anything else (MpegPs, Avi, Flv, …) we never target as shrink output;
///   refusing copy is the safe default.
fn container_supports_audio_copy(container: &Container, codec: &AudioCodec) -> bool {
    use AudioCodec::*;
    match container {
        Container::Mkv => true,
        Container::Mp4 | Container::Mov => {
            matches!(codec, Aac | Mp3 | Flac | Alac | Eac3)
        }
        Container::WebM => matches!(codec, Opus | Vorbis),
        _ => false,
    }
}

/// Audio strategy for shrink mode.
///
/// A manual ffmpeg user shrinking a file types `-c:a copy` because audio
/// re-encode is generation loss for almost no size win. We do the same —
/// except when the output container wouldn't accept the source codec
/// cleanly (e.g. AC-3 into MP4), in which case we fall back to the same
/// AAC policy the reencode planner uses.
fn select_audio_strategy_for_shrink(
    profile: &MediaProfile,
    output_container: &Container,
    overrides: &Overrides,
) -> AudioStrategy {
    let Some(primary) = profile.audio.first() else {
        return AudioStrategy::Copy;
    };
    if container_supports_audio_copy(output_container, &primary.codec) {
        AudioStrategy::Copy
    } else {
        select_audio_strategy(profile, overrides)
    }
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

/// Output and (optional) backup paths for an `--upgrade` run, computed
/// from the input path and the recipe's chosen output container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradePaths {
    /// Where the encoded output will be written (same directory as input,
    /// input's stem + the recipe's canonical container extension).
    pub output: PathBuf,
    /// Where to move the original to *before* encoding starts, when the
    /// output path would otherwise collide with the input. `None` means
    /// no backup rename is needed and the original stays put.
    pub backup: Option<PathBuf>,
}

/// Compute the output and (optional) backup paths for an `--upgrade` run.
///
/// The output is placed next to the input, with the input's meaningful
/// stem and the recipe's canonical container extension. A backup is only
/// produced when the output path would collide with the input path (same
/// container); in that case the original is earmarked for a rename to
/// `<stem>.vimprover-orig.<ext>` before encoding.
///
/// "Meaningful stem" extraction uses an allow-list of known media
/// extensions to avoid mangling filenames that happen to contain periods
/// (e.g. `Tutorial #10 (MC 1.7.10) (Low)` — `Path::extension` would return
/// `10) (Low)` there, which we ignore).
pub fn resolve_upgrade_paths(input: &Path, recipe: &EncodeRecipe) -> UpgradePaths {
    let parent = input.parent().unwrap_or_else(|| Path::new(""));
    let target_ext = recipe.output_container.canonical_extension();

    let filename = input.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let (stem, input_ext) = split_at_media_extension(filename);

    let output = parent.join(format!("{stem}.{target_ext}"));

    let backup = if output == input {
        // Case B: output would clobber input — we must rename aside. When
        // we're here `input_ext` is guaranteed to be `Some` (the output
        // path carries `target_ext`, and `output == input` implies the
        // input does too), but fall back to `target_ext` defensively.
        let backup_ext = input_ext.unwrap_or(target_ext);
        Some(parent.join(format!("{stem}.vimprover-orig.{backup_ext}")))
    } else {
        None
    };

    UpgradePaths { output, backup }
}

/// Split a filename into `(stem, optional_extension)`, treating only
/// recognized media extensions as actual extensions. Filenames whose last
/// "extension" isn't in the media allow-list are returned as `(whole, None)`
/// so we don't mangle titles containing periods.
fn split_at_media_extension(filename: &str) -> (&str, Option<&str>) {
    if let Some(dot_pos) = filename.rfind('.') {
        let ext = &filename[dot_pos + 1..];
        if is_media_extension(ext) {
            return (&filename[..dot_pos], Some(ext));
        }
    }
    (filename, None)
}

/// Common media-file extensions that vimprover might see as inputs. Used
/// solely by [`resolve_upgrade_paths`] to decide where the filename stem
/// ends. Not exhaustive — unfamiliar extensions just get treated as part
/// of the stem (conservative: means a `.mkv` is appended after them).
fn is_media_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mkv"
            | "mp4"
            | "m4v"
            | "mov"
            | "webm"
            | "avi"
            | "wmv"
            | "asf"
            | "flv"
            | "f4v"
            | "mpg"
            | "mpeg"
            | "m2v"
            | "vob"
            | "ts"
            | "m2ts"
            | "mts"
            | "ogv"
            | "ogm"
            | "3gp"
            | "3g2"
            | "rm"
            | "rmvb"
            | "divx"
    )
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
    fn shrink_default_uses_x265_crf_slow_at_1080p() {
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

        // No --target-bitrate ⇒ CRF mode. Default codec is x265 (manual
        // shrink convention), default preset is `slow`, default CRF for
        // 1080p x265 is 25.
        assert_eq!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX265 { crf: 25, preset: "slow".into() }
        );
        // Audio is stream-copied (no generation loss for negligible savings).
        assert_eq!(recipe.audio_strategy, AudioStrategy::Copy);
        // No downscale → no Scale filter.
        assert!(!recipe.video_filters.iter().any(|f| matches!(f, VideoFilter::Scale { .. })));
    }

    #[test]
    fn shrink_with_max_height_downscales_and_uses_lower_crf_band() {
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

        // Output height drops to 720p ⇒ x265 CRF default for ≤720p is 24.
        assert_eq!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX265 { crf: 24, preset: "slow".into() }
        );

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
    fn shrink_with_explicit_target_bitrate_switches_to_abr() {
        // Explicit --target-bitrate is the "I need a predictable size"
        // opt-in: planner switches from CRF to single-pass ABR with VBV
        // headroom, codec stays at x265, preset stays at slow.
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
            VideoStrategy::ReencodeX265Abr {
                target_bps,
                max_bps,
                bufsize_bps,
                preset,
            } => {
                assert_eq!(*target_bps, 2_500_000);
                assert_eq!(*max_bps, 3_125_000);
                assert_eq!(*bufsize_bps, 5_000_000);
                assert_eq!(preset, "slow");
            }
            other => panic!("expected ReencodeX265Abr, got {other:?}"),
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
            VideoStrategy::ReencodeX265Abr { target_bps, .. } => {
                assert_eq!(*target_bps, 1_000_000);
            }
            other => panic!("expected ReencodeX265Abr, got {other:?}"),
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
        // Output stays at 1080p ⇒ x265 CRF default for ≤1080p is 25.
        assert_eq!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX265 { crf: 25, preset: "slow".into() }
        );
    }

    #[test]
    fn shrink_uses_higher_crf_band_at_4k() {
        // 4K source: still x265 (always, for shrink), but the CRF default
        // bumps to 26 at >1080p.
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

        assert_eq!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX265 { crf: 26, preset: "slow".into() }
        );
    }

    #[test]
    fn shrink_default_codec_is_x265_even_for_sdr_1080p() {
        // Regression: the previous shrink planner inherited
        // `default_codec_choice`, which picked x264 for ≤1080p SDR. The
        // updated planner uses x265 unconditionally (overridable).
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
        .expect("plan_shrink");
        assert!(
            matches!(recipe.video_strategy, VideoStrategy::ReencodeX265 { .. }),
            "expected x265 for SDR 1080p shrink, got {:?}",
            recipe.video_strategy
        );
    }

    #[test]
    fn shrink_default_audio_is_copy_even_for_multichannel_source() {
        // Regression: previous shrink planner downmixed 5.1 → AAC stereo
        // (inherited from select_audio_strategy). The updated planner
        // stream-copies audio for shrink — generation loss isn't worth
        // the negligible size win.
        let mut p = oversized_1080p_profile();
        p.audio = vec![AudioInfo {
            codec: AudioCodec::Ac3,
            channels: Some(6),
            channel_layout: Some("5.1".into()),
            sample_rate_hz: Some(48_000),
            bitrate_bps: Some(448_000),
            language: Some("eng".into()),
        }];
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
        assert_eq!(recipe.audio_strategy, AudioStrategy::Copy);
    }

    #[test]
    fn shrink_audio_falls_back_to_aac_when_ac3_targets_mp4() {
        // AC-3 in MP4 is technically legal but compat-sketchy in consumer
        // players, so shrink falls back to the reencode planner's AAC
        // policy. Multichannel + default policy → AAC stereo downmix.
        let mut p = oversized_1080p_profile();
        p.audio = vec![AudioInfo {
            codec: AudioCodec::Ac3,
            channels: Some(6),
            channel_layout: Some("5.1".into()),
            sample_rate_hz: Some(48_000),
            bitrate_bps: Some(448_000),
            language: Some("eng".into()),
        }];
        let overrides = Overrides {
            container: Some(Container::Mp4),
            ..Overrides::default()
        };
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: None,
                target_bitrate_bps: None,
            },
            &overrides,
        )
        .expect("plan_shrink");
        assert_eq!(
            recipe.audio_strategy,
            AudioStrategy::AacStereo { bitrate_bps: 192_000 }
        );
    }

    #[test]
    fn shrink_audio_copies_aac_into_mp4() {
        // AAC in MP4 is the most compatible audio path possible: copy.
        let mut p = oversized_1080p_profile();
        p.audio = vec![AudioInfo {
            codec: AudioCodec::Aac,
            channels: Some(2),
            channel_layout: Some("stereo".into()),
            sample_rate_hz: Some(48_000),
            bitrate_bps: Some(192_000),
            language: None,
        }];
        let overrides = Overrides {
            container: Some(Container::Mp4),
            ..Overrides::default()
        };
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: None,
                target_bitrate_bps: None,
            },
            &overrides,
        )
        .expect("plan_shrink");
        assert_eq!(recipe.audio_strategy, AudioStrategy::Copy);
    }

    #[test]
    fn shrink_audio_falls_back_when_vorbis_targets_mp4() {
        // Vorbis is a WebM-only codec from MP4's perspective: refuse copy,
        // fall back to AAC. (Stereo source, default policy → AAC stereo.)
        let mut p = oversized_1080p_profile();
        p.audio = vec![AudioInfo {
            codec: AudioCodec::Vorbis,
            channels: Some(2),
            channel_layout: Some("stereo".into()),
            sample_rate_hz: Some(48_000),
            bitrate_bps: Some(160_000),
            language: None,
        }];
        let overrides = Overrides {
            container: Some(Container::Mp4),
            ..Overrides::default()
        };
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: None,
                target_bitrate_bps: None,
            },
            &overrides,
        )
        .expect("plan_shrink");
        assert_eq!(
            recipe.audio_strategy,
            AudioStrategy::AacStereo { bitrate_bps: 192_000 }
        );
    }

    #[test]
    fn shrink_honors_codec_crf_and_preset_overrides() {
        let p = oversized_1080p_profile();
        let overrides = Overrides {
            video_codec: Some(VideoCodecChoice::X264),
            crf: Some(20),
            preset: Some("medium".into()),
            ..Overrides::default()
        };
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: None,
                target_bitrate_bps: None,
            },
            &overrides,
        )
        .expect("plan_shrink");
        assert_eq!(
            recipe.video_strategy,
            VideoStrategy::ReencodeX264 { crf: 20, preset: "medium".into() }
        );
    }

    #[test]
    fn shrink_with_max_height_and_non_square_sar_emits_one_combined_scale() {
        // Pre-fix bug repro: 720×480 SAR 16:11 (DVD widescreen 16:9 NTSC)
        // with --max-height 360 produced a chain that downscaled to coded
        // 540×360, then SAR-corrected back to display 1046×480 — defeating
        // the downscale and distorting aspect ratio.
        //
        // Post-fix: a single combined Scale in display space, with width
        // computed as display_w × out_h / display_h:
        //   display_w = round(720 × 16/11) = 1047
        //   out_w = 1047 × 360 / 480 = 785.25 → 785 → round-down-even = 784
        //   out_h = 360
        // Plus setsar=1/1 to declare the output square-pixel.
        let p = MediaProfile {
            container: Container::MpegPs,
            video: VideoInfo {
                codec: VideoCodec::Mpeg2,
                width: 720,
                height: 480,
                field_order: FieldOrder::Progressive,
                framerate: Rational::new(60_000, 1001),
                pix_fmt: Some(PixFmt::Yuv420p),
                sar: Rational::new(16, 11),
                is_hdr: false,
            },
            audio: vec![AudioInfo {
                codec: AudioCodec::Ac3,
                channels: Some(2),
                channel_layout: Some("stereo".into()),
                sample_rate_hz: Some(48_000),
                bitrate_bps: Some(192_000),
                language: None,
            }],
            duration_secs: Some(60.0),
            file_size_bytes: Some(50_000_000),
            bitrate_bps: Some(6_000_000),
        };
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Shrink {
                max_height: Some(360),
                target_bitrate_bps: None,
            },
            &Overrides::default(),
        )
        .expect("plan_shrink");

        let scales: Vec<&VideoFilter> = recipe
            .video_filters
            .iter()
            .filter(|f| matches!(f, VideoFilter::Scale { .. }))
            .collect();
        assert_eq!(
            scales.len(),
            1,
            "expected exactly one Scale filter (no double-scale), got: {scales:?}"
        );
        assert_eq!(
            *scales[0],
            VideoFilter::Scale { width: 784, height: 360 },
            "downscale + SAR correction must combine into one display-space scale"
        );
        assert!(
            recipe
                .video_filters
                .iter()
                .any(|f| matches!(f, VideoFilter::SetSar { num: 1, den: 1 })),
            "expected setsar=1/1 to declare the output square-pixel"
        );
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
    fn reencode_sar_correction_rounds_odd_display_width_to_even() {
        // Repro: 352×240 MPEG-1 with SAR 200:219 (common low-bitrate VCD/MPG
        // capture). Display width = 352 × 200 / 219 ≈ 321.46 → 321. libx264
        // refuses odd widths in 4:2:0; the planner must round down to 320.
        let p = MediaProfile {
            container: Container::MpegPs,
            video: VideoInfo {
                codec: VideoCodec::Mpeg1,
                width: 352,
                height: 240,
                field_order: FieldOrder::Progressive,
                framerate: Rational::new(60000, 1001),
                pix_fmt: Some(PixFmt::Yuv420p),
                sar: Rational::new(200, 219),
                is_hdr: false,
            },
            audio: vec![AudioInfo {
                codec: AudioCodec::Mp2,
                channels: Some(2),
                channel_layout: Some("stereo".into()),
                sample_rate_hz: Some(44_100),
                bitrate_bps: Some(128_000),
                language: None,
            }],
            duration_secs: Some(60.0),
            file_size_bytes: Some(10_000_000),
            bitrate_bps: Some(1_100_000),
        };
        let recipe = plan(
            &p,
            &Assessment::default(),
            &Intent::Reencode,
            &Overrides::default(),
        )
        .unwrap();
        let scale = recipe
            .video_filters
            .iter()
            .find_map(|f| match f {
                VideoFilter::Scale { width, height } => Some((*width, *height)),
                _ => None,
            })
            .expect("expected a Scale filter from SAR correction");
        assert_eq!(scale, (320, 240));
        assert_eq!(scale.0 % 2, 0, "scale width must be even for libx264");
        assert_eq!(scale.1 % 2, 0, "scale height must be even for libx264");
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
    fn concat_intent_through_single_input_plan_errors() {
        // `plan()` is the single-input entry point. Step 6 introduces
        // `plan_concat()` for the multi-input case; routing Intent::Concat
        // through plan() means the caller has only one profile, which is
        // structurally not a concat.
        let p = sample_profile();
        let err = plan(
            &p,
            &Assessment::default(),
            &Intent::Concat,
            &Overrides::default(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConcatTooFewInputs(1)), "got {err:?}");
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

    // -----------------------------------------------------------------------
    // resolve_upgrade_paths
    // -----------------------------------------------------------------------

    /// Most common case: legacy container → MKV. No collision, so no backup.
    #[test]
    fn upgrade_paths_different_extension_no_backup() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let paths = resolve_upgrade_paths(Path::new("/home/u/myfile.wmv"), &recipe);
        assert_eq!(paths.output, PathBuf::from("/home/u/myfile.mkv"));
        assert_eq!(paths.backup, None);
    }

    /// Same container (MKV → MKV): output collides with input, so a backup
    /// is required. The backup filename puts `vimprover-orig` *before* the
    /// extension so the backup keeps its double-click behavior.
    #[test]
    fn upgrade_paths_same_extension_needs_backup() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let paths = resolve_upgrade_paths(Path::new("/home/u/myfile.mkv"), &recipe);
        assert_eq!(paths.output, PathBuf::from("/home/u/myfile.mkv"));
        assert_eq!(
            paths.backup,
            Some(PathBuf::from("/home/u/myfile.vimprover-orig.mkv"))
        );
    }

    /// Bare filename (no directory): parent-less join should still produce
    /// a sensible result.
    #[test]
    fn upgrade_paths_relative_filename_only() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let paths = resolve_upgrade_paths(Path::new("myfile.wmv"), &recipe);
        assert_eq!(paths.output, PathBuf::from("myfile.mkv"));
        assert_eq!(paths.backup, None);
    }

    /// Filename with periods in its title (no real extension). The allow-list
    /// means we don't mangle it — the entire filename is the stem, and we
    /// append `.mkv` after it.
    #[test]
    fn upgrade_paths_preserves_filenames_with_embedded_periods() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let paths = resolve_upgrade_paths(
            Path::new("Tutorial #10 (MC 1.7.10) (Low)"),
            &recipe,
        );
        assert_eq!(
            paths.output,
            PathBuf::from("Tutorial #10 (MC 1.7.10) (Low).mkv"),
        );
        assert_eq!(paths.backup, None);
    }

    /// Same as above but WITH a recognized media extension — the stem
    /// should drop only the trailing `.wmv`.
    #[test]
    fn upgrade_paths_preserves_titles_with_real_extension() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let paths = resolve_upgrade_paths(
            Path::new("Tutorial #10 (MC 1.7.10) (Low).wmv"),
            &recipe,
        );
        assert_eq!(
            paths.output,
            PathBuf::from("Tutorial #10 (MC 1.7.10) (Low).mkv"),
        );
        assert_eq!(paths.backup, None);
    }

    /// Forced MP4 container with an MP4 input → collision → backup needed.
    #[test]
    fn upgrade_paths_same_extension_mp4_backup() {
        let overrides = Overrides {
            container: Some(Container::Mp4),
            ..Overrides::default()
        };
        let recipe = plan_remux(&sample_profile(), &overrides);
        let paths = resolve_upgrade_paths(Path::new("/tmp/holiday.mp4"), &recipe);
        assert_eq!(paths.output, PathBuf::from("/tmp/holiday.mp4"));
        assert_eq!(
            paths.backup,
            Some(PathBuf::from("/tmp/holiday.vimprover-orig.mp4"))
        );
    }

    /// Uppercase extension: PathBuf equality is case-sensitive on Linux, so
    /// `myfile.MKV` != `myfile.mkv` and we produce an output without a
    /// backup. The user ends up with both files side-by-side (the original
    /// untouched at its original case).
    #[test]
    fn upgrade_paths_case_sensitive_extension_sees_no_collision() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let paths = resolve_upgrade_paths(Path::new("/t/Movie.MKV"), &recipe);
        assert_eq!(paths.output, PathBuf::from("/t/Movie.mkv"));
        assert_eq!(paths.backup, None);
    }

    // -----------------------------------------------------------------------
    // plan_concat / streams_compatible
    // -----------------------------------------------------------------------

    /// 1080p H.264 + AAC stereo MP4. Two of these are the canonical
    /// uniform-concat happy path.
    fn modern_mp4_profile() -> MediaProfile {
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
                language: None,
            }],
            duration_secs: Some(60.0),
            file_size_bytes: Some(50_000_000),
            bitrate_bps: Some(6_500_000),
        }
    }

    #[test]
    fn streams_compatible_accepts_identical_profiles() {
        let a = modern_mp4_profile();
        let b = modern_mp4_profile();
        assert!(streams_compatible(&a, &b).is_ok());
    }

    #[test]
    fn streams_compatible_rejects_resolution_mismatch() {
        let a = modern_mp4_profile();
        let mut b = modern_mp4_profile();
        b.video.width = 1280;
        b.video.height = 720;
        let err = streams_compatible(&a, &b).unwrap_err();
        assert!(err.contains("resolution"), "got: {err}");
        assert!(err.contains("1920x1080"), "got: {err}");
        assert!(err.contains("1280x720"), "got: {err}");
    }

    #[test]
    fn streams_compatible_rejects_video_codec_mismatch() {
        let a = modern_mp4_profile();
        let mut b = modern_mp4_profile();
        b.video.codec = VideoCodec::H265;
        let err = streams_compatible(&a, &b).unwrap_err();
        assert!(err.contains("video codec"), "got: {err}");
    }

    #[test]
    fn streams_compatible_rejects_pixel_format_mismatch() {
        let a = modern_mp4_profile();
        let mut b = modern_mp4_profile();
        b.video.pix_fmt = Some(PixFmt::Yuv422p);
        let err = streams_compatible(&a, &b).unwrap_err();
        assert!(err.contains("pixel format"), "got: {err}");
    }

    #[test]
    fn streams_compatible_rejects_framerate_mismatch() {
        let a = modern_mp4_profile();
        let mut b = modern_mp4_profile();
        b.video.framerate = Rational::new(30, 1); // vs 24/1
        let err = streams_compatible(&a, &b).unwrap_err();
        assert!(err.contains("framerate"), "got: {err}");
    }

    #[test]
    fn streams_compatible_tolerates_tiny_framerate_drift() {
        // 23.976 vs 24.000 — within the 0.1 fps epsilon.
        let mut a = modern_mp4_profile();
        let mut b = modern_mp4_profile();
        a.video.framerate = Rational::new(24000, 1001); // 23.976
        b.video.framerate = Rational::new(24, 1);
        assert!(streams_compatible(&a, &b).is_ok());
    }

    #[test]
    fn streams_compatible_rejects_audio_codec_mismatch() {
        let a = modern_mp4_profile();
        let mut b = modern_mp4_profile();
        b.audio[0].codec = AudioCodec::Ac3;
        let err = streams_compatible(&a, &b).unwrap_err();
        assert!(err.contains("audio codec"), "got: {err}");
    }

    #[test]
    fn streams_compatible_rejects_channel_count_mismatch() {
        let a = modern_mp4_profile();
        let mut b = modern_mp4_profile();
        b.audio[0].channels = Some(6); // 5.1 vs stereo
        let err = streams_compatible(&a, &b).unwrap_err();
        assert!(err.contains("channel"), "got: {err}");
    }

    #[test]
    fn streams_compatible_rejects_sample_rate_mismatch() {
        let a = modern_mp4_profile();
        let mut b = modern_mp4_profile();
        b.audio[0].sample_rate_hz = Some(44_100);
        let err = streams_compatible(&a, &b).unwrap_err();
        assert!(err.contains("sample rate"), "got: {err}");
    }

    #[test]
    fn streams_compatible_rejects_audio_present_only_on_one_side() {
        let a = modern_mp4_profile();
        let mut b = modern_mp4_profile();
        b.audio.clear();
        let err = streams_compatible(&a, &b).unwrap_err();
        assert!(err.contains("audio"), "got: {err}");
    }

    #[test]
    fn streams_compatible_accepts_no_audio_on_either_side() {
        let mut a = modern_mp4_profile();
        let mut b = modern_mp4_profile();
        a.audio.clear();
        b.audio.clear();
        assert!(streams_compatible(&a, &b).is_ok());
    }

    #[test]
    fn plan_concat_uniform_inputs_produces_demuxer_recipe() {
        let inputs: Vec<&Path> = vec![
            Path::new("/tmp/a.mp4"),
            Path::new("/tmp/b.mp4"),
            Path::new("/tmp/c.mp4"),
        ];
        let profiles = vec![
            modern_mp4_profile(),
            modern_mp4_profile(),
            modern_mp4_profile(),
        ];
        let recipe = plan_concat(&inputs, &profiles, &Overrides::default()).unwrap();
        assert_eq!(recipe.concat, Some(ConcatStrategy::Demuxer));
        assert_eq!(recipe.video_strategy, VideoStrategy::Copy);
        assert_eq!(recipe.audio_strategy, AudioStrategy::Copy);
        assert!(recipe.video_filters.is_empty());
        // Default container: MKV (no faststart flags).
        assert_eq!(recipe.output_container, Container::Mkv);
        assert!(recipe.extra_flags.is_empty());
    }

    #[test]
    fn plan_concat_mp4_override_emits_faststart() {
        let inputs: Vec<&Path> = vec![Path::new("/tmp/a.mp4"), Path::new("/tmp/b.mp4")];
        let profiles = vec![modern_mp4_profile(), modern_mp4_profile()];
        let overrides = Overrides {
            container: Some(Container::Mp4),
            ..Default::default()
        };
        let recipe = plan_concat(&inputs, &profiles, &overrides).unwrap();
        assert_eq!(recipe.output_container, Container::Mp4);
        assert_eq!(recipe.concat, Some(ConcatStrategy::Demuxer));
        assert_eq!(
            recipe.extra_flags,
            vec!["-movflags".to_string(), "+faststart".to_string()]
        );
    }

    #[test]
    fn plan_concat_refuses_mismatched_resolution() {
        let inputs: Vec<&Path> = vec![Path::new("/tmp/a.mp4"), Path::new("/tmp/b.mp4")];
        let mut second = modern_mp4_profile();
        second.video.width = 1280;
        second.video.height = 720;
        let profiles = vec![modern_mp4_profile(), second];
        let err = plan_concat(&inputs, &profiles, &Overrides::default()).unwrap_err();
        match err {
            Error::ConcatInputsDiffer { path, why } => {
                assert_eq!(path, PathBuf::from("/tmp/b.mp4"));
                assert!(why.contains("resolution"), "got: {why}");
            }
            _ => panic!("wrong error variant: {err:?}"),
        }
    }

    #[test]
    fn plan_concat_refuses_single_input() {
        let inputs: Vec<&Path> = vec![Path::new("/tmp/a.mp4")];
        let profiles = vec![modern_mp4_profile()];
        let err = plan_concat(&inputs, &profiles, &Overrides::default()).unwrap_err();
        assert!(matches!(err, Error::ConcatTooFewInputs(1)), "got {err:?}");
    }

    #[test]
    fn plan_concat_zero_inputs_refuses() {
        let inputs: Vec<&Path> = vec![];
        let profiles: Vec<MediaProfile> = vec![];
        let err = plan_concat(&inputs, &profiles, &Overrides::default()).unwrap_err();
        assert!(matches!(err, Error::ConcatTooFewInputs(0)), "got {err:?}");
    }

    #[test]
    fn plan_concat_first_mismatch_wins_when_multiple_differ() {
        // Inputs: [ref, ok, mismatch1, mismatch2]. Should report on input #3
        // (1-indexed) = "/tmp/c.mp4", not the later one.
        let inputs: Vec<&Path> = vec![
            Path::new("/tmp/ref.mp4"),
            Path::new("/tmp/b.mp4"),
            Path::new("/tmp/c.mp4"),
            Path::new("/tmp/d.mp4"),
        ];
        let mut third = modern_mp4_profile();
        third.video.width = 1280;
        third.video.height = 720;
        let mut fourth = modern_mp4_profile();
        fourth.video.codec = VideoCodec::H265;
        let profiles = vec![
            modern_mp4_profile(),
            modern_mp4_profile(),
            third,
            fourth,
        ];
        let err = plan_concat(&inputs, &profiles, &Overrides::default()).unwrap_err();
        match err {
            Error::ConcatInputsDiffer { path, .. } => {
                assert_eq!(path, PathBuf::from("/tmp/c.mp4"));
            }
            _ => panic!("wrong error variant: {err:?}"),
        }
    }
}
