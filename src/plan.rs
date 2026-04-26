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
use crate::model::{Container, MediaProfile};

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
    pub video_codec: Option<VideoStrategy>,
    pub crf: Option<u8>,
    pub max_height: Option<u32>,
    pub keep_multichannel_audio: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoStrategy {
    Copy,
    ReencodeX264 { crf: u8, preset: String },
    ReencodeX265 { crf: u8, preset: String },
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
/// Only [`Intent::Auto`] and [`Intent::Remux`] are implemented in build-order
/// step 2; other intents return [`Error::Unimplemented`].
pub fn plan(
    profile: &MediaProfile,
    _assessment: &Assessment,
    intent: &Intent,
    overrides: &Overrides,
) -> Result<EncodeRecipe> {
    match intent {
        Intent::Auto | Intent::Remux => Ok(plan_remux(profile, overrides)),
        Intent::Reencode => Err(Error::Unimplemented(
            "re-encode intent (build-order step 3)",
        )),
        Intent::Shrink { .. } => Err(Error::Unimplemented(
            "shrink intent (build-order step 5)",
        )),
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

/// Turn the user-supplied output name into a concrete path, using the
/// recipe's container extension when the user didn't supply one.
pub fn resolve_output_path(user_output: &Path, recipe: &EncodeRecipe) -> PathBuf {
    if user_output.extension().is_some() {
        user_output.to_path_buf()
    } else {
        user_output.with_extension(recipe.output_container.canonical_extension())
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
    fn auto_intent_plans_mkv_remux() {
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

    #[test]
    fn reencode_intent_is_unimplemented() {
        let p = sample_profile();
        let err = plan(
            &p,
            &Assessment::default(),
            &Intent::Reencode,
            &Overrides::default(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Unimplemented(_)), "got {err:?}");
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
    fn resolve_output_path_preserves_user_extension() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let out = resolve_output_path(Path::new("newname.mp4"), &recipe);
        assert_eq!(out, PathBuf::from("newname.mp4"));
    }

    #[test]
    fn resolve_output_path_handles_paths_with_directories() {
        let recipe = plan_remux(&sample_profile(), &Overrides::default());
        let out = resolve_output_path(Path::new("/tmp/subdir/movie"), &recipe);
        assert_eq!(out, PathBuf::from("/tmp/subdir/movie.mkv"));
    }
}
