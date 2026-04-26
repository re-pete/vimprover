//! Step 2 of the pipeline (stub): decide whether a probed file needs work.
//!
//! A file is "fine" (refuse to touch without `--force`) if all of these hold:
//!
//! - Video codec ∈ {H.264, H.265, AV1, VP9}
//! - Audio codec ∈ {AAC, Opus, FLAC, AC-3-in-MKV, E-AC-3-in-MKV}
//! - Container ∈ {MP4, MKV, WebM}
//! - For MP4: faststart (moov before mdat)
//! - For MKV: has Cues element
//! - Bitrate is reasonable for resolution (configurable thresholds)
//! - Progressive (not interlaced)
//! - Pixel format is yuv420p or yuv420p10le
//!
//! The actual checks will be filled in at Step 4 of the build order.

use crate::model::{AudioCodec, Container, MediaProfile, VideoCodec};

/// A single thing that makes a file "not fine".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    LegacyVideoCodec(VideoCodec),
    LegacyAudioCodec(AudioCodec),
    LegacyContainer(Container),
    /// MP4 with moov at end, MKV without Cues, AVI, MpegPs, etc.
    MissingSeekIndex,
    Interlaced,
    /// Pixel format not in {yuv420p, yuv420p10le}.
    NonModernPixelFormat,
    BitrateExcessive {
        actual: u64,
        threshold: u64,
    },
    /// e.g. 4K source with already-low bitrate-per-pixel (shrink mode).
    ResolutionWasteful,
    NonSquarePixels,
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
}

/// Run every "is this file fine?" check against `profile`.
///
/// TODO(step-4): implement every rule enumerated above.
pub fn assess(_profile: &MediaProfile) -> Assessment {
    Assessment::default()
}
