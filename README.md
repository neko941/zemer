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
| RIFF/WAVE PCM 8/16/24/32-bit | Supported | Read and write; decodes ordered `data` and `LIST/wavl` `data`/`slnt` segments. All 23 registered RIFF INFO text fields, BWF `bext`, cue-point `cue `, and sampler-loop `smpl` metadata read/write; `rewrite_metadata_with_bext` updates or adds `bext` while preserving audio and other chunks. Typed chunks are bounded to 16 MiB each. INFO honors common `CSET` code pages (default 1004); new INFO output declares UTF-8, and metadata edits preserve the declared code page. Unchanged INFO values stay byte-exact during edits. Extensible PCM accepts 1..=container valid bits, preserves container-width sample values and optional channel masks. |
| RIFF/WAVE IEEE float 32/64-bit | Supported | Read and write; non-PCM output includes `fmt ` extension and `fact` sample count. All 23 registered RIFF INFO fields, `cue `, and `smpl` metadata read/write. WAVE_FORMAT_EXTENSIBLE float and optional channel masks are preserved. |
| RIFF/WAVE G.711 A-law and μ-law | Supported | Read and write; non-PCM output includes `fmt ` extension and `fact` sample count. All 23 registered RIFF INFO fields, `cue `, and `smpl` metadata read/write. Decodes to signed 16-bit PCM; writing requires explicit G.711 selection. |
| RIFF/WAVE IMA ADPCM | Supported | 4-bit decoding and encoding for 1–8 channels in bounded WAVE blocks; multichannel output uses WAVE_FORMAT_EXTENSIBLE and the compressed-block sample-count field. Validates block geometry and `fact` sample count. CLI output uses `--format wav_ima_adpcm`; decoded samples are signed 16-bit PCM. |
| RIFF/WAVE Microsoft ADPCM | Supported | Mono and stereo 4-bit decoding with bounded format-defined coefficient tables and encoding with Microsoft’s standard seven; validates block geometry and trims padding with `fact`. FFmpeg 7.0.2 confirms sample-exact mono/stereo interoperability in both directions. CLI output uses `--format wav_ms_adpcm`. |
| RF64/WAVE | Supported | Read and write; all 23 registered RIFF INFO fields, `cue `, and `smpl` metadata read/write. Supports 64-bit file and data sizes; cue fields retain their format-defined 32-bit offsets. Select explicitly for output. |
| [Sun/NeXT AU](crates/au/) | Partial | Streams signed PCM 8/16/24/32-bit, IEEE float32/64, G.711 A-law/μ-law, and mono 8 kHz G.721 and G.723 3-bit/5-bit ADPCM. AU headers and linear PCM are big-endian; G.721 starts in the low nibble. Sun's original G.72x decoder confirms low-bit-first G.723 codewords. On 4,096 codewords per rate, Zemer matches an isolated ITU-T G.191 reconstructed-signal reference after applying the Annex A limiter at both G.723 rates; FFmpeg and libsndfile use differing PCM output behavior on these vectors. Reads and writes bounded raw annotations, maps valid UTF-8 to COMMENT, and rewrites metadata without decoding audio. Eight McGill PCM/float/G.711 profiles match FFmpeg sample-for-sample; the malformed McGill offset and size samples are rejected. G.723 requires complete eight-sample groups. See the roadmap for remaining AU compatibility work. |
| [Core Audio Format (CAF)](crates/caf/) | Partial | Reads/writes CAF v1 signed PCM 16/24/32-bit and IEEE float32/64 in either byte order, packed/unpacked 24-bit PCM, G.711 A/μ-law, Apple IMA4, UTF-8 `info` tags, and bitmap channel layouts. Metadata edits preserve encoded audio packets. An FFmpeg 8.1-generated big-endian PCM vector is checked sample-for-sample. Requires integral sample rates representable by shared `AudioSpec`; multichannel files need a supported channel bitmap. AAC, ALAC, MACE, and custom channel-description layouts remain unsupported. |
| [AIFF](crates/aiff/) | Supported | Reads and writes classic AIFF integer PCM from 1–32 valid bits. Native text and typed `COMT`, `MARK`, and `INST` chunks are available; the profile passes McGill/AFsp and generated FFmpeg interoperability tests at 1–8 channels. |
| [AIFF-C / AIFC](crates/aiff/) | Partial | Reads and writes big-endian PCM (`NONE`/`twos`), little-endian PCM (`sowt`), IEEE float, G.711, Apple IMA4, DWVW (2–32 bits, 1–8 channels), and 8 kHz G.726 (16/24/32/40 kbit/s). IMA4 mono/stereo and generated 16-bit `sowt` output match FFmpeg sample-for-sample; 8/16/24/32-bit `sowt` byte layouts and round trips are tested. The DWVW writer round trips all depths across 1–8 channels; metadata-only rewrites preserve unknown chunks and encoded audio bytes. Supports typed `COMT`/`MARK`/`INST`, opaque `SAXL`/`AESD`, native text, and the non-standard ID3 compatibility chunk with APIC/PIC picture metadata. SAXL payloads are retained but not used to seek. G.722/G.728/GSM remain behind IPR review; ACE/MACE lack complete clean-room algorithms and independent vectors. Broader corpus work and independent full-container stereo DWVW interoperability remain open. |
| CUE sheets for WAVE/AUDIO tracks | Supported | Read and write; track title and performer fields. Split referenced WAV audio into track WAVs and create CUE sheets for WAV tracks. |
| ISO 9660 images | Supported | Inspect and stream-extract single-volume images with primary or Joliet names. Safe extraction rejects traversal, unsafe names, and case-colliding paths. This does not read SACD DSD ISO structures. |
| Native FLAC | Supported | Streaming integer PCM encode (8/16/24/32-bit, verbatim or fixed-predictor Rice-coded frames) and decode with frame CRC and STREAMINFO MD5 verification; sample seeking for seekable readers (SEEKTABLE accelerated, forward-scan fallback); Vorbis comment rewrite with raw metadata blocks preserved; PICTURE blocks read/write through shared `Metadata`, plus CUESHEET, SEEKTABLE, and APPLICATION parsing. CLI inspection, conversion to WAVE/FLAC, and comment editing are available. |
| Ogg-FLAC | Supported | Streaming decode and encode through the reusable `zemer-ogg` page/packet reader/writer; reads the current mapping and the tested pre-1.1.1 legacy packet layout. Validates terminal granules including valid nonzero initial offsets, trims final-frame samples, and decodes chained links even when their audio specifications differ; `spec()` and each PCM block report the active link's specification. Seekable readers can seek by restarting and decoding forward. PICTURE metadata round-trips through shared `Metadata`; comment editing normalizes legacy mappings and preserves FLAC audio packets. CLI inspection/conversion handles chains with one common audio specification; metadata editing updates the first link. |
| Ogg Vorbis (`.ogg`/`.oga`) | Experimental | Clean-room Float32 decode, chained streams, comment rewriting, and linear sample-frame seeking for seekable inputs. CLI inspection/conversion and metadata edits are available. Decoder fixtures cover Xiph/libVorbis and two independent Rust encoders, including six-channel input, plus full-length CC0, CC BY 4.0, and public-domain recordings and a public-domain percussion transient checked sample-by-sample against independent PCM oracles; encoding remains experimental pending perceptual listening and wider conformance. |
| WavPack | Supported | Streaming decode covers lossless PCM, IEEE float32, hybrid `.wvc` restoration, and native DSD profiles. Lossless integer PCM encoding supports 1–32 channels at 8/16/24/32 bits; float32 is bit-exact but stored uncompressed in WVX; raw native DSD encoding supports 1–32 channels. Integer PCM hybrid encoding writes standard-bitrate `.wv`+`.wvc` pairs; APEv2 text and JPEG/PNG/GIF/BMP/WebP/TIFF front/back cover art are supported by the library. CLI inspect, conversion, and text metadata editing are supported, and metadata edits preserve embedded cover art. The official suite decoded 35 supported files (including 2 DSD and 16 correction-paired files), skipped 9 legacy files, and rejected all 4 malformed files. |
| Headerless raw PCM | Supported | Caller supplies sample format, sample rate, and channel count. Streams U8, signed 16/24/32-bit PCM, and IEEE float32/64 in explicit byte order. CLI inspect/conversion uses `--input-format`, `--sample-rate`, `--channels`, and `--format raw_*`; partial final sample frames are rejected. |
| [DSF](crates/dsf/) | Supported | Reads and writes Sony DSF 1.01 raw DSD in both one-bit and eight-bit storage order for mono through 5.1 at 2.8224/5.6448 MHz; reads common uncompressed ID3v2.2/v2.3/v2.4 text and APIC/PIC pictures and writes ID3v2.4 UTF-8 text and pictures. DSF-to-DSF conversion preserves complete bounded ID3 tags; metadata edits retain pictures and opaque frames and copy encoded audio unchanged. ID3v2.2's reserved compression flag means the tag is ignored as a whole, per that version's specification. |
| [DSDIFF / DFF](crates/dsdiff/) | Partial | Reads raw DSD across the full 1–65,535-channel DSDIFF `CHNL` field range and compressed DST (up to 64 channels at DSD64/128/256), validates optional frame CRCs, and writes raw DSD or compressed DST using a clean-room first-order predictor with valid mode-0 bypass fallback. Supports title, artist, general and typed comments, channel IDs, typed DIIN/MARK records, and audio/unknown-chunk-preserving metadata edits. All three complete 300-frame `DST_02_02` vectors pass bit-exactly; the first 10 frames of every available ISO vector also pass. Independent marker/DST interoperability coverage and patent/legal clearance remain open. |

## Roadmap

See [ROADMAP.md](ROADMAP.md) for planned formats and workflows.

The WAV reader concatenates top-level `data` and `LIST/wavl` audio segments in file order, including RF64 `ds64` size-table entries. `slnt` holds the preceding interleaved sample frame; initial silence uses neutral PCM/G.711 codes. IMA and Microsoft ADPCM decoding requires a `fact` sample count and trims final-block padding; their byte-rate fields are advisory while block geometry is validated. Microsoft ADPCM coefficient tables are bounded to 256 entries; the encoder emits the standard seven coefficients. The reader caps retained audio segments at one million and supported INFO metadata at 16 MiB total, 65,536 values, and 1 MiB per value. Typed `bext`, `cue `, and `smpl` chunks are bounded to 16 MiB each. The writer emits a conventional contiguous `data` chunk. Other compressed codecs inside WAV are not implemented. The writer defaults to classic RIFF/WAVE, using WAVE_FORMAT_EXTENSIBLE when a channel mask is provided and for multichannel IMA ADPCM; select `WavContainer::Rf64` or CLI `--container rf64` for 64-bit sizes. Cue-point fields remain 32-bit as specified, including in RF64 files. Classic RIFF reports an explicit error before exceeding its 4 GiB size limit.

## Workspace crates

- [`zemer-core`](crates/core/): shared decoder/encoder interfaces; re-exports metadata, PCM, and DSD types for convenience.
- [`zemer-cue`](crates/cue/) (Rust crate `cue`): clean-room WAVE/AUDIO CUE parser and writer with checked sector/sample-frame conversion; UTF-8/Shift-JIS input is capped at 16 MiB.
- [`zemer-dsd`](crates/dsd/): reusable native DSD stream specification and packed octet buffers.
- [`zemer-dst`](crates/dst/): reusable clean-room DST frame decoder and first-order encoder for DSD64/128/256 profiles.
- [`zemer-dsdiff`](crates/dsdiff/) (Rust crate `dsdiff`): raw and compressed DST DSDIFF `.dff` decoder, plus raw DSD and compressed DST writers with bypass fallback.
- [`zemer-dsf`](crates/dsf/) (Rust crate `dsf`): DSF 1.01 raw one-bit and eight-bit DSD decoder and encoder for the documented profile, with common ID3 text and APIC/PIC picture mapping.
- [`zemer-flac`](crates/flac/) (Rust crate `flac`): native/Ogg-FLAC streaming integer PCM encoders and decoders, native and Ogg seeking, Vorbis-comment rewriting, and raw metadata-block access with typed PICTURE/CUESHEET/SEEKTABLE/APPLICATION access. Native and Ogg-FLAC PICTURE blocks map to the shared `Metadata::pictures()` API and can be encoded from it.
- [`zemer-vorbis`](crates/vorbis/): clean-room Ogg Vorbis I Float32 decoder and experimental fixed-precision and target-bitrate encoders; see the experimental status above for validation limits.
- [`zemer-metadata`](crates/metadata/): format-independent tags and embedded-picture values shared by audio formats; native and Ogg-FLAC currently map pictures through the shared API.
- [`zemer-id3`](crates/id3/): bounded ID3v2 text and APIC/PIC picture parsing, encoding, and metadata rewriting shared by DSF and AIFF/AIFC.
- [`zemer-iso9660`](crates/iso9660/): clean-room ISO 9660 image inspection and safe single-volume extraction, including Joliet names.
- [`zemer-ogg`](crates/ogg/): bounded Ogg page/packet reader and writer with page CRC, sequence, and continuation validation.
- [`zemer-pcm`](crates/pcm/): reusable PCM sample types and sample-format conversion.
- [`zemer-raw-pcm`](crates/raw-pcm/): headerless PCM reader and writer with explicit format, byte order, and bounded streaming.
- [`zemer-wav`](crates/wav/) (Rust crate `wav`): RIFF/WAVE PCM/float/G.711, 1–8 channel IMA ADPCM, and mono/stereo Microsoft ADPCM reader/writer.
- [`zemer-ima-adpcm`](crates/ima-adpcm/): shared clean-room IMA ADPCM predictor and Apple IMA4 packet codec used by WAVE, AIFC, and CAF.
- [`zemer-ms-adpcm`](crates/ms-adpcm/): shared clean-room Microsoft ADPCM block decoder and encoder.
- [`zemer-aiff`](crates/aiff/) (Rust crate `aiff`): AIFF/AIFC PCM, float, G.711, IMA4, DWVW, and G.726 reader/writer profile, including typed `COMT`/`MARK`/`INST`, raw AIFC `AESD`, and non-standard ID3 chunk metadata; integer PCM writers support 1–32 valid bits via the library API and AIFF/AIFC CLI `--valid-bits`.
- [`zemer-au`](crates/au/) (Rust crate `au`): streaming Sun/NeXT AU signed PCM, float, G.711, and mono 8 kHz G.721 reader/writer with bounded annotation preservation.
- [`zemer-caf`](crates/caf/) (Rust crate `caf`): CAF v1 PCM/float, G.711, and Apple IMA4 reader/writer with bounded UTF-8 `info` tags and packet-preserving metadata edits.
- [`zemer-g711`](crates/g711/) (Rust crate `g711`): reusable ITU-T G.711 A-law/μ-law sample conversion used by WAVE, AIFF-C, and CAF.
- [`zemer-dwvw`](crates/dwvw/): clean-room DWVW channel encoder and decoder used by AIFC.
- [`zemer-wavpack`](crates/wavpack/) (Rust crate `wavpack`): PCM/native DSD decoding, lossless integer PCM, standard-bitrate integer PCM hybrid `.wv`+`.wvc` encoding, bit-exact float32, raw native DSD encoding, APEv2 text and front/back picture metadata reading and rewriting, and checksum validation.
- [`zemer`](crates/zemer/): CLI package and executable for inspection, conversion including `wav_ima_adpcm` and `wav_ms_adpcm`, DSF/DSDIFF/native DSD WavPack transcoding, WAVE/AIFF/AU/CAF/DSF/DSDIFF/FLAC/Ogg-FLAC/Ogg Vorbis/WavPack metadata editing, CUE/WAV workflows, and ISO 9660 inspection/extraction. CLI code is organized under `crates/zemer/src/cli/` by audio conversion, metadata editing, and reporting.
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
cargo run -p zemer -- convert --format aifc_alaw input.wav voice-alaw.aifc
cargo run -p zemer -- convert --format aifc_mulaw input.wav voice-mulaw.aifc
cargo run -p zemer -- convert --format aifc_ima4 input.wav voice-ima4.aifc
cargo run -p zemer -- convert --format aifc_g726 --bitrate 40 input-8khz.wav voice-g726.aifc
cargo run -p zemer -- convert --format aifc_dwvw input.wav voice-dwvw.aifc
cargo run -p zemer -- convert --format aifc_sowt input.wav little-endian.aifc
cargo run -p zemer -- convert --format aiff --valid-bits 12 input.wav output.aiff
cargo run -p zemer -- convert --format flac input.wav output.flac
cargo run -p zemer -- convert --format ogg_flac input.wav output.oga
cargo run -p zemer -- convert --format vorbis input.wav output.ogg
```

Copy native DSD between containers without passing through PCM. Select
``dsdiff_dst`` to write compressed DST frames where the predictor reduces the
frame, with valid bypass frames otherwise:

```sh
cargo run -p zemer -- convert --format dsf input.dff output.dsf
cargo run -p zemer -- convert --format dsdiff_dst input.dsf output.dff
cargo run -p zemer -- convert --format wavpack input.dsf output.wv
```

Edit RIFF INFO and BWF `bext` fields in WAVE, AIFF/AIFC text chunks, native/Ogg-FLAC/Ogg Vorbis comments,
or WavPack APEv2 text tags while keeping unmentioned tags. Repeated `--set` values for the same key create repeated
fields; `--remove` removes all values for a key. The output path must be new.
WAVE BWF edits also accept UMID and reserved fields as fixed-length hexadecimal
values through `--bext-umid-hex` and `--bext-reserved-hex`.
Metadata editing preserves encoded audio without decoding or re-encoding it:

```sh
zemer metadata edit --set "TITLE=New title" --set "ARTIST=Artist" --remove COMMENT input.flac updated.flac --json
zemer metadata edit --set "TITLE=New title" input.wav updated.wav --json
zemer metadata edit --bext-description "Final mix" --bext-time-reference 12345 input.wav updated.wav --json
zemer metadata edit --set "TITLE=New title" input.aiff updated.aiff --json
zemer metadata edit --set "TITLE=New title" input.dsf updated.dsf --json
zemer metadata edit --set "TITLE=New title" input.dff updated.dff --json
zemer metadata edit --set "TITLE=New title" input.oga updated.oga --json
zemer metadata edit --set "TITLE=New title" input.ogg updated.ogg --json
```

Split a WAVE-backed CUE sheet to numbered track WAVs, or create a CUE for WAV
tracks. The split command creates a new output directory and refuses to
overwrite existing files.

```sh
zemer cue split album.cue album-tracks
zemer cue create album.cue "album-tracks/01 - Intro.wav" "album-tracks/02 - Song.wav"
```

Conversion preserves supported RIFF INFO tags. It refuses to overwrite an existing output file, rounds integer quantization to the nearest sample, and errors when float samples are non-finite or outside `[-1, 1]` for integer output. Float input converted to native FLAC, Ogg-FLAC, or AIFF becomes 24-bit integer PCM; AIFC can retain IEEE float. No dither is applied.

## Supported platforms

Windows and Linux are the supported targets. CI runs tests on both platforms.

## Agent and CLI support

The `zemer` command is scriptable and provides stable JSON reports and errors on stderr with a non-zero exit code. Any agent that can run shell commands can use it.

## Development

Run `cargo test --workspace` and `cargo fmt --all -- --check`. CI checks Linux and Windows targets. The Linux reference test host is `server5`.

## License

Licensed under either MIT or Apache-2.0, at your option.
