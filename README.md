# Zemer

<p align="center">
  <a href="#supported-platforms"><img alt="Windows" src="https://img.shields.io/badge/Windows-supported-blue.svg" /></a>
  <a href="#supported-platforms"><img alt="Linux" src="https://img.shields.io/badge/Linux-supported-blue.svg" /></a>
  <a href="#cli"><img alt="Agent-friendly CLI" src="https://img.shields.io/badge/CLI-agent--friendly-blueviolet.svg" /></a>
  <a href="https://github.com/neko941/zemer/tags"><img alt="Version" src="https://img.shields.io/github/v/tag/neko941/zemer?sort=semver&amp;label=version" /></a>
</p>

Zemer is a clean-room Rust library for reading, writing, inspecting, and converting music files. Its target platforms are Windows and Linux. The code is developed from published format specifications and original tests; proprietary application binaries are not used as implementation sources, and GPL implementation code is not copied or linked.

The workspace separates format-independent audio types from each format implementation. Applications can depend on only the crates they need.

## Current support

| Format | Status | Capabilities |
|---|---|---|
| RIFF/WAVE PCM 8/16/24/32-bit | Supported | Read and write; RIFF INFO metadata read/write. Extensible PCM accepts 1..=container valid bits, preserves container-width sample values and optional channel masks. |
| RIFF/WAVE IEEE float 32/64-bit | Supported | Read and write; RIFF INFO metadata read/write. WAVE_FORMAT_EXTENSIBLE float and optional channel masks are preserved. |
| RIFF/WAVE G.711 A-law and μ-law | Supported | Read and write; RIFF INFO metadata read/write. Decodes to signed 16-bit PCM; writing requires explicit G.711 selection. |
| RF64/WAVE | Supported | Read and write; RIFF INFO metadata read/write. Supports 64-bit file and data sizes; select explicitly for output. |
| CUE sheets for WAVE/AUDIO tracks | Supported | Read and write; track title and performer fields. Split referenced WAV audio into track WAVs and create CUE sheets for WAV tracks. |
| Native FLAC | Partial | Streaming read with frame CRC and STREAMINFO MD5 verification when the checksum is present and the stream is fully consumed; Vorbis comment rewrite with raw metadata blocks preserved; PICTURE, CUESHEET, SEEKTABLE, and APPLICATION fields parsed. CLI inspection, conversion to WAVE, and comment editing are available; FLAC audio encoding is not implemented. |
| WavPack | Partial | Streaming decode of mono/stereo lossless integer PCM at 8/16/24/32 bits, stereo lossless IEEE float32, and 5.1 lossless PCM16; mono/stereo unshaped hybrid-lossy PCM16; tested stereo noise-shaped hybrid-lossy PCM16; tested stereo PCM16 hybrid-lossless restoration with `.wvc`, both flat and noise-shaped. Cross-channel `.wvc` correction, broader multichannel layouts, and DSD remain unsupported. |

## Roadmap

See [ROADMAP.md](ROADMAP.md) for planned formats and workflows.

The WAV reader handles the first `data` chunk and skips unknown RIFF/RF64 chunks, including RF64 `ds64` size-table entries. `wavl`/`slnt` and non-G.711 compressed codecs inside WAV are not implemented. The writer defaults to classic RIFF/WAVE (using WAVE_FORMAT_EXTENSIBLE when a channel mask is provided); select `WavContainer::Rf64` or CLI `--container rf64` for 64-bit sizes. Classic RIFF reports an explicit error before exceeding its 4 GiB size limit.

## Workspace crates

- [`zemer-core`](crates/core/): shared decoder/encoder interfaces; re-exports metadata and PCM types for convenience.
- [`zemer-cue`](crates/cue/) (Rust crate `cue`): CUE sheet types and checked sector/sample-frame conversion.
- [`zemer-flac`](crates/flac/) (Rust crate `flac`): native FLAC streaming decoder and metadata rewriter with raw metadata-block access and typed PICTURE/CUESHEET/SEEKTABLE/APPLICATION access.
- [`zemer-metadata`](crates/metadata/): format-independent tags shared by audio formats.
- [`zemer-pcm`](crates/pcm/): reusable PCM sample types and sample-format conversion.
- [`zemer-wav`](crates/wav/) (Rust crate `wav`): RIFF/WAVE reader and writer.
- [`zemer-wavpack`](crates/wavpack/) (Rust crate `wavpack`): bounded block inspection; mono/stereo lossless integer PCM, stereo float32, 5.1 PCM16, mono/stereo unshaped hybrid-lossy PCM16, tested stereo shaped hybrid-lossy PCM16, and tested stereo PCM16 hybrid-lossless decoding with `.wvc` correction files in flat and shaped modes. Cross-channel `.wvc` correction is rejected explicitly.
- [`zemer`](crates/zemer/): CLI package and executable for inspection, conversion, metadata editing, and CUE/WAV workflows.
- [`tests/`](tests/): workspace-level integration tests; codec parser unit tests stay with their format crate.

## Documentation

User-facing Sphinx documentation lives directly in `docs/` and is configured for Read the Docs with `.readthedocs.yaml`.
See [ROADMAP.md](ROADMAP.md) for planned format, metadata, conversion, and disc-image work.

## Example

```rust,no_run
use std::fs::File;
use zemer_core::AudioDecoder;
use wav::WavDecoder;

let mut decoder = WavDecoder::new(File::open("input.wav")?)?;
while let Some(block) = decoder.read_frames(4096)? {
    // Pass each PCM block to a DSP step or an encoder.
    println!("{} frames", block.frames());
}
# Ok::<(), zemer_core::AudioError>(())
```

## CLI

Install the latest version from the `main` branch with Cargo (Rust must be installed):

```sh
  cargo install --git https://github.com/neko941/zemer.git --branch main --locked zemer
```

This installs the `zemer` command in Cargo's binary directory, usually `~/.cargo/bin`.

Inspect a WAV file in stable machine-readable JSON (useful in scripts and agent workflows):

```sh
cargo run -p zemer -- inspect --json input.wav
```

Errors are emitted as a JSON object on stderr with a non-zero exit status when `--json` is set.
The inspection JSON includes `speaker_mask` as a number or `null` so scripts can retain known channel positions.

Convert to another supported WAVE sample format or explicitly encode G.711 (no resampling or channel remixing):

```sh
cargo run -p zemer -- convert --format pcm_i16 input.wav output.wav
cargo run -p zemer -- convert --format pcm_i16 --container rf64 input.wav large-output.wav
cargo run -p zemer -- convert --format g711_alaw input.wav voice-alaw.wav
cargo run -p zemer -- convert --format g711_mulaw input.wav voice-mulaw.wav
```

Edit RIFF INFO tags in WAVE or Vorbis comments in native FLAC while keeping
unmentioned tags. Repeated `--set` values for the same key create repeated
fields; `--remove` removes all values for a key. The output path must be new.
Metadata editing copies encoded audio without decoding or re-encoding it:

```sh
zemer metadata edit --set "TITLE=New title" --set "ARTIST=Artist" --remove COMMENT input.flac updated.flac --json
zemer metadata edit --set "TITLE=New title" input.wav updated.wav --json
```

Split a WAVE-backed CUE sheet to numbered track WAVs, or create a CUE for WAV
tracks. The split command creates a new output directory and refuses to
overwrite existing files.

```sh
zemer cue split album.cue album-tracks
zemer cue create album.cue "album-tracks/01 - Intro.wav" "album-tracks/02 - Song.wav"
```

Conversion preserves supported RIFF INFO tags. It refuses to overwrite an existing output file, rounds integer quantization to the nearest sample, and errors when float samples are non-finite or outside `[-1, 1]` for integer output. No dither is applied.

## Supported platforms

Windows and Linux are the supported targets. CI runs tests on both platforms.

## Agent and CLI support

The `zemer` command is scriptable and provides stable JSON reports and errors on stderr with a non-zero exit code. Any agent that can run shell commands can use it.

## Development

Run `cargo test --workspace` and `cargo fmt --all -- --check`. CI checks Linux and Windows targets. The Linux reference test host is `server5`.

## License

Licensed under either MIT or Apache-2.0, at your option.
