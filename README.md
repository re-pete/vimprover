# vimprover

A command-line tool that intelligently re-encodes, repairs, and modernizes
media files. It probes input files, decides what (if anything) needs to be done
to bring them up to modern standards, and either does it or explains why it
won't.

The full design — including the build order this work follows — lives in
[`CLAUDE.md`](./CLAUDE.md).

## Status

**Step 7: polish, round 1 — done.** The full single-file pipeline
(probe → assess → plan → format → execute), single-file shrink,
multi-input concat, and the first polish round all work end-to-end.

**Concat mode (≥2 inputs).** Triggers demuxer-based stream-copy
concatenation. The planner probes every input, verifies they share
codec / resolution / pixel format / framerate / audio parameters, and —
when uniform — joins them via ffmpeg's `-f concat` demuxer with no
re-encoding (typically 10× faster than re-encoding). Non-uniform inputs
are refused with a precise error naming the offending input and the
field that differs, plus a hint to normalize via `vimprover --reencode`
or `--intent shrink` first. The filter-concat re-encode-to-common-spec
path is deferred until there's demand.

**Polish highlights (round 1):**

- **Atomic output.** ffmpeg writes to `<output>.partial.<ext>` and
  vimprover atomically renames it to the final path on success. A
  crash or Ctrl-C halfway through never leaves a corrupt file at the
  user-visible path. Stale partials from a prior failed run are
  refused until cleared (either manually or with `--overwrite`).
- **Completion summary.** Post-encode line shows input→output sizes,
  percent change, and wall-clock duration — e.g.
  `Done. Wrote out.mkv (847 MiB → 312 MiB, 63% smaller, 8m12s).`
- **Organized `--help`.** Flags grouped into *Run control / Intent /
  Encoding* sections, with a verbose `--help` that includes a worked
  examples block and an environment variables section.

**`--upgrade`: upgrade-in-place.** A single-input mode that auto-computes
the output path from the input and the chosen container. The original is
preserved either by sitting at its original path (when the container
changes — `myfile.wmv` and the new `myfile.mkv` end up side-by-side) or
by being renamed aside to `<stem>.vimprover-orig.<ext>` before encoding
(when the container stays the same). On any encode failure, the rename
is rolled back so the user sees the original at its original path.
Refuses to clobber pre-existing outputs or stale backups without
`--overwrite`.

**Everything from earlier steps still applies:** `assess()`, the
`Issues:` block, the `[Y/n]` prompt unless `--yes`, the fine-gate
(refuses already-fine files in `Auto` mode without `--force`), intent
overrides, and `--intent shrink` with bitrate-excessive /
resolution-wasteful flagging. Shrink re-encodes with x265 CRF by
default; single-pass ABR is only used when `--target-bitrate` is given.

**Progress:** ffmpeg runs with `-loglevel warning`; its output goes
directly to the terminal. No progress bar yet — that is deferred.

## Requirements

- Rust toolchain (edition 2024 — `rustc >= 1.85`)
- `ffmpeg` and `ffprobe` on `$PATH`, or pointed at via `VIMPROVER_FFMPEG` /
  `VIMPROVER_FFPROBE`

## Build

```sh
cargo build           # debug
cargo build --release # optimized
```

## Run

```sh
# Upgrade-in-place. No OUTPUT path needed: vimprover writes next to
# the input (myfile.wmv → myfile.mkv) and preserves the original
# unchanged at its original path. Recommended for most uses.
cargo run -- --upgrade myfile.wmv

# Same idea, but reencode an MKV that needs improvement. The original
# is renamed aside to myfile.vimprover-orig.mkv before encoding starts;
# rolled back if anything fails.
cargo run -- --upgrade --reencode myfile.mkv

# Auto-modernize with an explicit OUTPUT name. assess() decides: legacy
# codec/interlacing ⇒ re-encode; legacy container only ⇒ remux. Prompts
# for confirmation.
cargo run -- old-movie.vob newname

# Skip the [Y/n] prompt (required for non-TTY use):
cargo run -- --yes old-movie.vob newname

# Process a file even if assess() reports it as already fine:
cargo run -- --force --yes already-modern.mkv newname

# Force a re-encode regardless of assessment:
cargo run -- --yes --reencode old-movie.vob newname

# Shrink: cap bitrate at the per-resolution threshold (DWIM):
cargo run -- --yes --intent shrink huge-1080p.mkv smaller

# Shrink with downscale to 720p:
cargo run -- --yes --intent shrink --max-height 720 huge-1080p.mkv smaller

# Shrink with an explicit target bitrate (suffix 'M' = Mbps):
cargo run -- --yes --intent shrink --target-bitrate 2.5M huge.mkv smaller

# Bare --max-height also implies shrink:
cargo run -- --yes --max-height 1080 4k-source.mkv smaller-1080p

# Force x265 + MP4, custom CRF, slower preset for better compression:
cargo run -- --yes --reencode --video-codec x265 --container mp4 \
    --crf 22 --preset slow old-movie.vob newname

# Preserve 5.1 audio instead of downmixing to stereo:
cargo run -- --yes --reencode --keep-multichannel-audio dvd-rip.vob newname

# Just print the probe summary (step-1 functionality):
cargo run -- --probe-only some-movie.mkv

# Print the plan and exact ffmpeg command, but don't run it:
cargo run -- --dry-run old-movie.vob newname

# Overwrite an existing output file:
cargo run -- --yes --overwrite old-movie.vob newname

# Verbose tracing (logs the exact ffprobe/ffmpeg commands at INFO):
VIMPROVER_LOG=info cargo run -- --yes old-movie.vob newname

# Use a non-PATH ffmpeg / ffprobe:
VIMPROVER_FFPROBE=/opt/ffmpeg/bin/ffprobe \
VIMPROVER_FFMPEG=/opt/ffmpeg/bin/ffmpeg \
    cargo run -- --yes old-movie.vob newname

# Concat: multiple inputs → single output. Inputs must already be
# uniform (same codec/resolution/audio); vimprover refuses with a clear
# error otherwise.
cargo run -- --yes part1.mp4 part2.mp4 part3.mp4 joined

# Same operation with the explicit intent (handy for scripts):
cargo run -- --yes --intent concat clip1.mp4 clip2.mp4 joined.mkv
```

Example output (Auto on a DVD-shaped MPEG-PS source):

```
File:      old-rip.vob
Container: MPEG-PS, 4.2 GiB, 1h32m00s, 6.5 Mbps
Video:     MPEG-2, 720x480 (display 655x480), interlaced (TFF), 29.970 fps, yuv420p, SAR 10:11
Audio 1:   AC-3, 5.1(side), 48 kHz, 448 kbps (eng)

Issues:    legacy video codec (MPEG-2)
           legacy container (MPEG-PS)
           interlaced source
           non-square pixels (SAR 10:11)

Plan:      Re-encode video to H.264 (CRF 20, medium preset) in Matroska (MKV)
           Deinterlace with bwdif
           Scale to 654x480
           Set square pixel aspect ratio
           Downmix audio to AAC stereo 192 kbps
           Output: newname.mkv

Proceed? [Y/n] y

Running ffmpeg…
…
Done. Wrote newname.mkv (4.2 GiB → 2.1 GiB, 50% smaller, 18m42s).
```

## Test

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

Unit tests feed canned ffprobe JSON through the parser and construct recipes
by hand, so they run without `ffprobe` or `ffmpeg`. Integration tests in
`tests/remux.rs` do shell out to ffmpeg (to synthesize a VOB-like test file
and verify the round-trip); they skip with a printed notice if ffmpeg is
missing.

## Layout

```
src/
├── lib.rs       # crate root; declares the modules below
├── main.rs      # binary entry point (probe → plan → render → execute)
├── cli.rs       # clap Args + value enums (binary-only)
├── error.rs     # typed Error / Result
├── model.rs     # MediaProfile, VideoInfo, codec/container/pixfmt enums
├── probe.rs     # ffprobe runner + JSON → MediaProfile
├── format.rs    # human-readable rendering of profiles & recipes
├── assess.rs    # "is this file fine?" check (Issue, Assessment, assess())
├── plan.rs      # MediaProfile → EncodeRecipe (remux, re-encode, shrink, concat)
└── execute.rs   # build ffmpeg argv, run it (atomic partial→rename), surface errors

tests/
├── remux.rs          # end-to-end integration tests (needs ffmpeg)
└── fixtures/         # synthetic test videos at various resolutions

scripts/
└── shrink4k.fish     # fish function: scan directories and batch-shrink >1080p files
```

## License

Dual-licensed under MIT or Apache-2.0, at your option.
