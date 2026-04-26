# vimprover

A command-line tool that intelligently re-encodes, repairs, and modernizes
media files. It probes input files, decides what (if anything) needs to be done
to bring them up to modern standards, and either does it or explains why it
won't.

The full design — including the build order this work follows — lives in
[`CLAUDE.md`](./CLAUDE.md).

## Status

**Build-order step 3: single-file re-encode.** `vimprover --reencode INPUT OUTPUT`
plans a re-encode (H.264/H.265 with sensible defaults, deinterlace,
square-pixel correction, AAC audio downmix), prints the plan, and runs
ffmpeg. The default invocation (no `--reencode`) still stream-copy-remuxes—
`Intent::Auto` will flip to re-encode automatically once the assessment
lands in step 4. `--probe-only` preserves step 1's diagnostic mode.

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
# Remux a legacy file to MKV (planner picks .mkv since no extension given):
cargo run -- old-movie.vob newname

# Force a re-encode (H.264 by default, with deinterlace + square-pixel fix
# applied automatically when the source needs them):
cargo run -- --reencode old-movie.vob newname

# Force x265 + MP4, custom CRF, slower preset for better compression:
cargo run -- --reencode --video-codec x265 --container mp4 \
    --crf 22 --preset slow old-movie.vob newname

# Preserve 5.1 audio instead of downmixing to stereo:
cargo run -- --reencode --keep-multichannel-audio dvd-rip.vob newname

# Just print the probe summary (step-1 functionality):
cargo run -- --probe-only some-movie.mkv

# Print the plan and exact ffmpeg command, but don't run it:
cargo run -- --dry-run --reencode old-movie.vob newname

# Overwrite an existing output file:
cargo run -- --overwrite old-movie.vob newname

# Verbose tracing (logs the exact ffprobe/ffmpeg commands at INFO):
VIMPROVER_LOG=info cargo run -- old-movie.vob newname

# Use a non-PATH ffmpeg / ffprobe:
VIMPROVER_FFPROBE=/opt/ffmpeg/bin/ffprobe \
VIMPROVER_FFMPEG=/opt/ffmpeg/bin/ffmpeg \
    cargo run -- old-movie.vob newname
```

Example output:

```
File:      old-rip.vob
Container: MPEG-PS, 4.2 GiB, 1h32m00s, 6.5 Mbps
Video:     MPEG-2, 720x480 (display 655x480), interlaced (TFF), 29.970 fps, yuv420p, SAR 10:11
Audio 1:   AC-3, 5.1(side), 48 kHz, 448 kbps (eng)

Plan:      Remux to Matroska (MKV) (stream copy, no re-encode)
           Output: newname.mkv

Running ffmpeg…
frame= 2760 fps=5520 q=-1.0 Lsize=  4304640KiB time=01:32:00.00 bitrate=6538.2kbits/s speed=3.35e+03x
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
├── assess.rs    # "is this file fine?" (stub; step 4)
├── plan.rs      # MediaProfile → EncodeRecipe (remux + re-encode; shrink/concat stubbed)
└── execute.rs   # build ffmpeg argv, run it, surface errors

tests/
└── remux.rs     # end-to-end integration tests (needs ffmpeg)
```

## License

Dual-licensed under MIT or Apache-2.0, at your option.
