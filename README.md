# Zemer

<p align="center">
  <a href="#supported-platforms"><img alt="Windows" src="https://img.shields.io/badge/Windows-supported-blue.svg" /></a>
  <a href="#supported-platforms"><img alt="Linux" src="https://img.shields.io/badge/Linux-supported-blue.svg" /></a>
  <a href="#agent-and-cli-support"><img alt="Claude Code" src="https://img.shields.io/badge/Claude_Code-CLI--friendly-blueviolet.svg" /></a>
  <a href="#agent-and-cli-support"><img alt="Codex" src="https://img.shields.io/badge/Codex-CLI--friendly-blueviolet.svg" /></a>
  <a href="#agent-and-cli-support"><img alt="Cursor" src="https://img.shields.io/badge/Cursor-CLI--friendly-blueviolet.svg" /></a>
  <a href="#agent-and-cli-support"><img alt="Kimi Code" src="https://img.shields.io/badge/Kimi_Code-CLI--friendly-blueviolet.svg" /></a>
</p>

Zemer is a clean-room Rust library for reading, writing, inspecting, and converting music files. Its target platforms are Windows and Linux. The code is developed from published format specifications and original tests; proprietary application binaries are not used as implementation sources, and GPL implementation code is not copied or linked.

The workspace separates format-independent audio types from each format implementation. Applications can depend on only the crates they need.

## Current support

| Format | Read | Write | Metadata | Status |
|---|---:|---:|---:|---|
| RIFF/WAVE PCM 8/16/24/32-bit | Yes | Yes | RIFF INFO read/write | Extensible PCM accepts 1..=container valid bits, preserves container-width sample values and optional channel masks |
| RIFF/WAVE IEEE float 32/64-bit | Yes | Yes | RIFF INFO read/write | WAVE_FORMAT_EXTENSIBLE float and optional channel masks are preserved |
| RIFF/WAVE G.711 A-law and μ-law | Yes | Yes | RIFF INFO read/write | Decodes to signed 16-bit PCM; writing requires explicit G.711 selection |
| RF64/WAVE | Yes | Yes | RIFF INFO read/write | 64-bit file and data sizes; select explicitly for output |
| DTS Coherent Acoustics (`.dts`) | No | No | No | Planned; core decode and DTS-HD scope to be evaluated |
| Nero disc image (`.nrg`) | No | No | No | Planned; disc-image evaluation, not an audio codec |
| FLAC, AIFF, CAF, AU, ALAC, MP3, AAC, Ogg/Vorbis, Opus, APE, WavPack, WMA, TTA | No | No | No | Planned |
| DSF, DSDIFF/DFF, SACD/DSD ISO (including DST) | No | No | No | Planned; independent spec-based work |

“Planned” does not mean supported yet. Format work will be added only with implementation and conformance tests.

The WAV reader handles the first `data` chunk and skips unknown RIFF/RF64 chunks, including RF64 `ds64` size-table entries. `wavl`/`slnt` and non-G.711 compressed codecs inside WAV are not implemented. The writer defaults to classic RIFF/WAVE (using WAVE_FORMAT_EXTENSIBLE when a channel mask is provided); select `WavContainer::Rf64` or CLI `--container rf64` for 64-bit sizes. Classic RIFF reports an explicit error before exceeding its 4 GiB size limit.

## Workspace crates

- `zemer-core`: format-independent PCM buffers, metadata tags, and the decoder interface.
- `zemer-wav`: RIFF/WAVE reader and writer.
- `zemer-cli`: optional `zemer` executable for inspection and explicit sample-format conversion; library crates do not depend on it.
- `tests/`: workspace-level integration tests; codec parser unit tests stay with their format crate.

## Documentation

User-facing Sphinx documentation lives directly in `docs/` and is configured for Read the Docs with `.readthedocs.yaml`.
See [ROADMAP.md](ROADMAP.md) for planned format, metadata, conversion, and disc-image work.

## Example

```rust,no_run
use std::fs::File;
use zemer_core::AudioDecoder;
use zemer_wav::WavDecoder;

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
cargo install --git https://github.com/neko941/zemer.git --branch main --package zemer-cli --locked
```

This installs the `zemer` command in Cargo's binary directory, usually `~/.cargo/bin`.

Inspect a WAV file in stable machine-readable JSON (useful in scripts and agent workflows):

```sh
cargo run -p zemer-cli -- inspect --json input.wav
```

Errors are emitted as a JSON object on stderr with a non-zero exit status when `--json` is set.
The inspection JSON includes `speaker_mask` as a number or `null` so scripts can retain known channel positions.

Convert to another supported WAVE sample format or explicitly encode G.711 (no resampling or channel remixing):

```sh
cargo run -p zemer-cli -- convert --format pcm_i16 input.wav output.wav
cargo run -p zemer-cli -- convert --format pcm_i16 --container rf64 input.wav large-output.wav
cargo run -p zemer-cli -- convert --format g711_alaw input.wav voice-alaw.wav
cargo run -p zemer-cli -- convert --format g711_mulaw input.wav voice-mulaw.wav
```

Conversion preserves supported RIFF INFO tags. It refuses to overwrite an existing output file, rounds integer quantization to the nearest sample, and errors when float samples are non-finite or outside `[-1, 1]` for integer output. No dither is applied.

## Supported platforms

Windows and Linux are the supported targets. CI runs tests on both platforms.

## Agent and CLI support

The `zemer` command is scriptable and provides stable JSON for inspection and conversion reports, with JSON errors on stderr and a non-zero exit code. Agents that can run shell commands can use it, including Claude Code, Codex, Cursor, and Kimi Code. The badges describe CLI compatibility; dedicated agent plugins are not included.

## Development

Run `cargo test --workspace` and `cargo fmt --all -- --check`. CI checks Linux and Windows targets. The Linux reference test host is `server5`.

## License

Licensed under either MIT or Apache-2.0, at your option.
