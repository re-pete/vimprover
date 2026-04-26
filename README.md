# vimprover

A command-line tool that intelligently re-encodes, repairs, and modernizes
media files. It probes input files, decides what (if anything) needs to be done
to bring them up to modern standards, and either does it or explains why it
won't.

The full design — including the build order this work follows — lives in
[`CLAUDE.md`](./CLAUDE.md).

## Status

**Build-order step 4: assessment + interactive prompt.** `vimprover INPUT OUTPUT`
now probes the input, runs an `assess()` pass that flags every issue (legacy
codec, container, audio codec, interlacing, non-modern pixel format,
non-square pixels), prints them under an `Issues:` block, then routes
`Intent::Auto` to either remux or re-encode based on what was found. Files
that pass every check are refused with a friendly "already fine" message —
pass `--force` to process them anyway.

All user-facing destructive actions go through a `[Y/n]` prompt (default
yes) unless `--yes` is passed. Non-TTY stdin requires `--yes` explicitly
so batch scripts have to opt into non-interactive runs.

Explicit intents — `--reencode` (and the eventual `--intent` flag) —
bypass the fine-gate and the prompt is still shown. `--probe-only` and
`--dry-run` short-circuit before the prompt.

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
# Auto-modernize a legacy file. assess() decides: legacy codec/interlacing
# ⇒ re-encode; legacy container only ⇒ remux. Prompts for confirmation.
cargo run -- old-movie.vob newname

# Skip the [Y/n] prompt (required for non-TTY use):
cargo run -- --yes old-movie.vob newname

# Process a file even if assess() reports it as already fine:
cargo run -- --force --yes already-modern.mkv newname

# Force a re-encode regardless of assessment:
cargo run -- --yes --reencode old-movie.vob newname

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
Done. Wrote newname.mkv.
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
├── plan.rs      # MediaProfile → EncodeRecipe (remux + re-encode; shrink/concat stubbed)
└── execute.rs   # build ffmpeg argv, run it, surface errors

tests/
└── remux.rs     # end-to-end integration tests (needs ffmpeg)
```

## License

Dual-licensed under MIT or Apache-2.0, at your option.
