//! `vimprover` — intelligently re-encode, repair, and modernize media files.
//!
//! This crate is organized around the pipeline laid out in `CLAUDE.md`:
//!
//! 1. [`probe`]   — run ffprobe, parse into a typed [`model::MediaProfile`].
//! 2. [`assess`]  — pure function `MediaProfile → Assessment`.
//! 3. [`plan`]    — pure function `(MediaProfile, Assessment, Intent, Overrides) → EncodeRecipe`.
//! 4. [`format`]  — render a recipe / profile for humans.
//! 5. [`execute`] — spawn ffmpeg, stream progress, handle errors.
//!
//! Only step 1 and the rendering half of step 4 are implemented today; the
//! remaining modules expose typed skeletons so higher-level code can be wired
//! up incrementally.

#![warn(clippy::all)]
#![warn(missing_debug_implementations)]

pub mod assess;
pub mod error;
pub mod execute;
pub mod format;
pub mod model;
pub mod plan;
pub mod probe;

pub use error::{Error, Result};
pub use model::MediaProfile;
