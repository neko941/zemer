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
| RIFF/WAVE PCM 8/16/24/32-bit | Supported | Read and write; decodes ordered `data` and `LIST/wavl` `data`/`slnt` segments. RIFF INFO metadata read/write. Extensible PCM accepts 1..=container valid bits, preserves container-width sample values and optional channel masks. |
| RIFF/WAVE IEEE float 32/64-bit | Supported | Read and write; RIFF INFO metadata read/write. WAVE_FORMAT_EXTENSIBLE float and optional channel masks are preserved. |
| RIFF/WAVE G.711 A-law and μ-law | Supported | Read and write; RIFF INFO metadata read/write. Decodes to signed 16-bit PCM; writing requires explicit G.711 selection. |
| RF64/WAVE | Supported | Read and write; RIFF INFO metadata read/write. Supports 64-bit file and data sizes; select explicitly for output. |
| CUE sheets for WAVE/AUDIO tracks | Supported | Read and write; track title and performer fields. Split referenced WAV audio into track WAVs and create CUE sheets for WAV tracks. |
| Native FLAC | Supported | Streaming integer PCM encode (8/16/24/32-bit, verbatim or fixed-predictor Rice-coded frames) and decode with frame CRC and STREAMINFO MD5 verification; sample seeking for seekable readers (SEEKTABLE accelerated, forward-scan fallback); Vorbis comment rewrite with raw metadata blocks preserved; PICTURE, CUESHEET, SEEKTABLE, and APPLICATION fields parsed. CLI inspection, conversion to WAVE/FLAC, and comment editing are available. |
| Ogg-FLAC | Supported | Streaming decode and encode through the reusable `zemer-ogg` page/packet reader/writer; validates terminal granules, trims final-frame samples, and supports CLI inspection and conversion to WAVE/Ogg-FLAC. One logical stream per file; Ogg seeking and chained logical streams are not included. |
| WavPack | Supported | Streaming decode covers lossless PCM, IEEE float32, hybrid `.wvc` restoration, and native DSD profiles. Lossless integer PCM encoding supports 1–32 channels at 8/16/24/32 bits; float32 is bit-exact but stored uncompressed in WVX; raw native DSD encoding supports 1–32 channels. Integer PCM hybrid encoding writes standard-bitrate `.wv`+`.wvc` pairs; APEv2 text tags, CLI inspect, conversion, and metadata editing are supported. The official suite decoded 35 supported files (including 2 DSD and 16 correction-paired files), skipped 9 legacy files, and rejected all 4 malformed files. |
| [DSF](crates/dsf/) | Partial | `zemer-dsf` reads and writes Sony DSF 1.01 raw one-bit DSD for supported layouts at 2.8224/5.6448 MHz; it reads ID3v2.3/v2.4 and writes common ID3v2.4 text tags. CLI inspect and bit-exact DSF ↔ native DSD WavPack conversion are supported. Artwork/unknown-frame preservation remains open. |

## Roadmap

See [ROADMAP.md](ROADMAP.md) for planned formats and workflows.

The WAV reader concatenates top-level `data` and `LIST/wavl` audio segments in file order, including RF64 `ds64` size-table entries. `slnt` holds the preceding interleaved sample frame; initial silence uses neutral PCM/G.711 codes. The reader caps retained audio segments at one million. The writer emits a conventional contiguous `data` chunk. Non-G.711 compressed codecs inside WAV are not implemented. The writer defaults to classic RIFF/WAVE (using WAVE_FORMAT_EXTENSIBLE when a channel mask is provided); select `WavContainer::Rf64` or CLI `--container rf64` for 64-bit sizes. Classic RIFF reports an explicit error before exceeding its 4 GiB size limit.

## Workspace crates

- [`zemer-core`](crates/core/): shared decoder/encoder interfaces; re-exports metadata, PCM, and DSD types for convenience.
- [`zemer-cue`](crates/cue/) (Rust crate `cue`): CUE sheet types and checked sector/sample-frame conversion.
- [`zemer-dsd`](crates/dsd/): reusable native DSD stream specification and packed octet buffers.
- [`zemer-dsf`](crates/dsf/) (Rust crate `dsf`): DSF 1.01 raw one-bit DSD decoder and encoder for the documented profile, with common ID3 text-tag mapping.
- [`zemer-flac`](crates/flac/) (Rust crate `flac`): native FLAC streaming integer PCM encoder, decoder, seek API, and metadata rewriter with raw metadata-block access and typed PICTURE/CUESHEET/SEEKTABLE/APPLICATION access.
- [`zemer-metadata`](crates/metadata/): format-independent tags shared by audio formats.
- [`zemer-ogg`](crates/ogg/): bounded Ogg page/packet reader and writer with page CRC, sequence, and continuation validation.
- [`zemer-pcm`](crates/pcm/): reusable PCM sample types and sample-format conversion.
- [`zemer-wav`](crates/wav/) (Rust crate `wav`): RIFF/WAVE reader and writer.
- [`zemer-wavpack`](crates/wavpack/) (Rust crate `wavpack`): PCM/native DSD decoding, lossless integer PCM, standard-bitrate integer PCM hybrid `.wv`+`.wvc` encoding, bit-exact float32, raw native DSD encoding, APEv2 text metadata reading and rewriting, and checksum validation.
- [`zemer`](crates/zemer/): CLI package and executable for inspection, conversion, DSF/native DSD WavPack transcoding, WAVE/FLAC/WavPack metadata editing, and CUE/WAV workflows.
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
cargo run -p zemer -- convert --format flac input.wav output.flac
cargo run -p zemer -- convert --format ogg_flac input.wav output.oga
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
