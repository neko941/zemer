# Zemer roadmap

Zemer aims to be a modular, clean-room Rust toolkit for music audio, metadata,
conversion, and optical-disc workflows on Windows and Linux. This document
describes intended capabilities, not release dates or current support. The
[README support matrix](README.md#current-support) is authoritative for what
works today.

Implementations will use published specifications and original test vectors.
We will not copy proprietary or GPL implementation code. Every format is
supported only after its readers or writers, documented limits, and meaningful
tests land. Extensions are hints: the container and audio codec must be
identified from file contents.

## What the library is intended to do

- **Inspect:** identify containers and codecs, report audio properties, tracks,
  duration, metadata, and known limitations without decoding an entire file.
- **Decode and encode:** stream supported audio formats into and out of shared
  sample types, with explicit format and quality choices.
- **Convert:** move between supported formats; make resampling, channel mixing,
  dithering, and lossy encoding explicit options rather than hidden behavior.
- **Edit metadata:** read, add, change, and remove supported tags while
  preserving unknown fields or clearly reporting when a rewrite cannot retain
  them.
- **Work with collections and discs:** safely list, test, create, and extract
  ZIP archives; read and write cue sheets; inspect, create, and extract supported
  ISO and BIN/CUE images.
- **Automate:** provide a small optional CLI with stable JSON output, useful
  errors, batch processing, progress reporting, and overwrite safeguards.

## 1. WAV foundation — in progress

- Complete robust RIFF/WAVE reading and writing for PCM 8/16/24/32-bit, IEEE
  float 32/64-bit, and G.711 A-law/μ-law.
- Preserve optional WAVE extensible channel masks without inventing a speaker
  layout; add stronger channel-layout types only when multiple formats need
  them.
- [x] Read and write RF64 with 64-bit RIFF/data sizes and `ds64` table handling
  for oversized chunks.
- Add `wavl` and `slnt` segmented audio support.
- Expand metadata beyond RIFF INFO where specifications and tests justify it,
  including broadcast, cue, sampler, and embedded metadata chunks.
- Improve malformed-file, boundary-size, unusual-header, and G.711 vector
  regression coverage.

## 2. Shared audio, conversion, and metadata

- Keep streaming decoder and encoder APIs independent of format-specific crates.
- [x] Extract the format-independent tag collection into the standalone
  ``zemer-metadata`` crate; ``zemer-core`` re-exports it for convenience.
- Establish common metadata fields and map them to each format's native tags;
  retain format-specific fields when they do not fit the shared model.
- Support metadata inspection and editing, including artwork and chapters when
  a format supports them.
- Add explicit resampling, channel mixing, and dithering options. Preserve
  sample rate and channel layout by default; never transform audio implicitly.
- Provide seeking, duration estimation, format capability reporting, and clear
  distinctions between lossless and lossy conversions where the format allows.
- For WavPack hybrid files, report whether output is lossy or losslessly
  corrected; never silently omit or mislabel the optional `.wvc` correction
  file.
- Preserve source precision where possible and report clipping, quantization,
  unsupported metadata, and lossy steps to callers and CLI users.

## 3. Audio containers and codecs

Each container and codec will be independently usable. A container can hold
different codecs, so support for an extension does not automatically imply
support for every file with that extension.

| Format | Intended read/decode work | Intended write/encode work | Priority |
|---|---|---|---|
| RIFF/WAVE and RF64 | Inspect, stream PCM/float/G.711, read metadata | Write PCM/float/G.711; edit RIFF INFO while preserving audio, unknown chunks, and RF64 sizes | Foundation |
| FLAC | Partial native FLAC streaming decode with frame CRC and STREAMINFO MD5 verification; Vorbis comments, raw metadata-block preservation, and typed PICTURE/CUESHEET/SEEKTABLE/APPLICATION reads | Metadata rewrite and CLI tag edits implemented; FLAC audio encoding remains planned | CLI inspect/WAVE conversion implemented; Ogg-FLAC and seeking remain open |
| AIFF / AIFF-C | Parse chunks, decode supported PCM/float and declared codecs | Write supported PCM/float, metadata | Lossless |
| AU / CAF / raw PCM | Decode supported encodings; raw PCM requires caller-supplied parameters | Encode supported encodings and raw PCM | Lossless / utility |
| WavPack (`.wv`) | Partial: mono/stereo lossless integer PCM at 8/16/24/32 bits, stereo lossless IEEE float32, 5.1 lossless PCM16, mono/stereo unshaped hybrid-lossy PCM16, and tested stereo PCM16 `.wvc` hybrid-lossless restoration. Next: noise-shaped hybrid, broader multichannel layouts, and DSD | Lossless encoding, DSD, and APEv2 tags remain planned | Clean-room specification path confirmed; implementation uses Rust `std` and Zemer crates only |
| MP3, AAC | Inspect, decode, metadata, and seek where supported | Encode with explicit profile/quality options | Licensing and build review first |
| DTS Coherent Acoustics (`.dts`) | Inspect and decode DTS core; evaluate DTS-HD and embedded/container streams separately | Evaluate clean-room encoding scope and licensing before promising a writer | Spec-based feasibility review ([ETSI TS 102 114](https://www.etsi.org/deliver/etsi_ts/102100_102199/102114/01.06.01_60/ts_102114v010601p.pdf)) |
| Ogg with Vorbis / Opus | Parse streams and decode each supported codec | Encode each codec and write stream metadata | Separate codec support |
| WMA and other legacy formats | Evaluate supported variants and dependencies | Encode only where clean-room scope and licensing permit | Evaluate separately |
| DSF / DSDIFF | Inspect, decode DSD, read format metadata | Write DSD and metadata where specified | DSD |
| SACD DSD ISO | Inspect areas/tracks, extract DSD or DST tracks and metadata | Create images only if specification-based workflows and tests are practical | DSD / disc |

Read and write are separate capabilities: a format may be readable before an
encoder is ready. Formats that depend on restricted, undocumented, or
platform-specific implementations may remain unsupported.

## 4. DSD and optical-disc audio

- Read DSF and DSDIFF/DFF metadata and audio; add writing where the
  specifications allow it.
- Inspect SACD/DSD ISO navigation, stereo and multichannel areas, track
  boundaries, timing, and metadata; extract supported tracks without loading an
  entire image into memory.
- Evaluate DST decoding as a separate codec with its own tests and documented
  limits.
- Preserve track, index, timing, and channel information when exporting tracks
  or creating cue sheets.
- Keep PCM conversion from DSD explicit, since it is a signal-processing step
  with configurable filtering and rate choices.
- Leave encrypted, protected, or undocumented content out of scope unless a
  lawful, specification-based path is available.

## 5. CUE/BIN, ISO, and ZIP workflows

These are related tools, not interchangeable audio formats:

- **ZIP:** list contents, test integrity, extract safely, and create archives.
  Prevent path traversal, make overwrite behavior explicit, and support large
  collections without unnecessary memory use.
- **CUE:** parse, validate, edit, and write track/index layouts and referenced
  file paths; report missing files and invalid offsets. The current WAVE/AUDIO
  split and create workflows are listed in the README support matrix; other
  track types and disc-image layouts remain planned.
- **BIN/CUE:** inspect and extract supported tracks; create images and cue
  sheets from supported source layouts while preserving track type, pregap, and
  index information where representable.
- **ISO:** inspect supported image filesystems and extract files; create an ISO
  from an explicit directory or disc layout when its filesystem rules are
  implemented and tested.
- **NRG (Nero image):** evaluate image inspection, audio/mixed-mode track
  extraction, and conversion to supported layouts. Nero describes NRG as its
  proprietary image format for uses that include audio CDs and mixed-mode
  discs ([Nero manual](https://www.nero.com/eng/downloads/test-files/test-pdf.pdf));
  implementation starts only if reliable format details and original test
  images are available. Treat image creation as a later, separately validated
  capability.
- **Useful combined workflows:** extract a ZIP that contains audio or disc
  files; package a completed CUE/BIN or ISO set into ZIP; create an ISO from an
  extracted directory; generate a CUE sheet alongside authored BIN track data;
  extract supported tracks from an image and convert them to supported audio
  formats.

A ZIP is a file archive, an ISO is a disc image, and CUE describes track layout.
There is no general byte-for-byte “ZIP to ISO/CUE” conversion. Zemer will offer
the valid steps between them (extract, inspect, arrange files or tracks, then
create the requested output) and will reject workflows that would silently
discard layout or audio information.

## 6. CLI, reliability, and platform support

- Keep the CLI optional and scriptable. Provide stable JSON for inspection,
  conversion, metadata, archive, and disc operations as those features land.
- Support batch workflows, useful progress for long operations, actionable
  errors, explicit overwrite behavior, and safe handling of partial outputs.
- Maintain Windows and Linux builds and tests. Review each dependency's license
  and build requirements before adding it.
- Add format conformance tests, original fixtures, parser fuzzing, and
  benchmarks for streaming and conversion paths as the implementation grows.
- Keep public documentation and the support matrix aligned with actual
  behavior; planned formats must never appear as supported prematurely.

## Current status

See the [README support matrix](README.md#current-support) for implemented
formats and exact read/write support. WAVE/RF64 and native FLAC decoding are
implemented, with WAVE-backed CUE track splitting and CUE creation. FLAC
writing, Ogg-FLAC, other codecs, track types, and archive/disc workflows remain
planned until code and tests land.
