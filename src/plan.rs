//! Step 3 of the pipeline (stub): pure function from
//! `(profile, assessment, intent, overrides) → EncodeRecipe`.
//!
//! Planner rules (from `CLAUDE.md`) to be implemented in build-order step 3:
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

use crate::assess::Assessment;
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

/// Build an [`EncodeRecipe`] from a profile and an assessment.
///
/// TODO(step-3): implement the planner decision logic.
pub fn plan(
    _profile: &MediaProfile,
    _assessment: &Assessment,
    _intent: &Intent,
    _overrides: &Overrides,
) -> EncodeRecipe {
    unimplemented!("planner lands in build-order step 3")
}
