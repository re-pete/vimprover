# vimprover

A command-line tool that intelligently re-encodes, repairs, and modernizes
media files. It probes input files, decides what (if anything) needs to be done
to bring them up to modern standards, and either does it or explains why it
won't.

The full design — including the build order this work follows — lives in
[`CLAUDE.md`](./CLAUDE.md).

## Status

**Build-order step 1: skeleton + probe.** `vimprover INPUT` runs `ffprobe`
against the input and prints a typed summary. The `assess`, `plan`, and
`execute` modules are typed skeletons; their behavior arrives in later steps.

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
cargo run -- some-movie.mkv
cargo run -- /path/to/clip1.flv /path/to/clip2.wmv

# Verbose internal tracing (e.g., the exact ffprobe command):
VIMPROVER_LOG=debug cargo run -- some-movie.mkv

# Use a non-PATH ffprobe:
VIMPROVER_FFPROBE=/opt/ffmpeg/bin/ffprobe cargo run -- some-movie.mkv
```

Example output:

```
File:      old-rip.vob
Container: MPEG-PS, 4.2 GiB, 1h32m00s, 6.5 Mbps
Video:     MPEG-2, 720x480 (display 655x480), interlaced (TFF), 29.970 fps, yuv420p, SAR 10:11
Audio 1:   AC-3, 5.1(side), 48 kHz, 448 kbps (eng)
```

## Test

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

Tests are pure — they feed canned ffprobe JSON to the parser, so `ffprobe`
itself is not required to run them.

## Layout

```
src/
├── lib.rs       # crate root; declares the modules below
├── main.rs      # binary entry point (clap + tokio + probe + print)
├── cli.rs       # clap Args (binary-only)
├── error.rs     # typed Error / Result
├── model.rs     # MediaProfile, VideoInfo, codec/container/pixfmt enums
├── probe.rs     # step 1: ffprobe runner + JSON → MediaProfile
├── format.rs    # human-readable rendering of profiles (and later, recipes)
├── assess.rs    # step 2 (stub): "is this file fine?"
├── plan.rs      # step 3 (stub): MediaProfile → EncodeRecipe
└── execute.rs   # step 5 (stub): run ffmpeg, stream progress
```

## License

Dual-licensed under MIT or Apache-2.0, at your option.
