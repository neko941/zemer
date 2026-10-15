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
- [x] Decode ordered top-level `data` and `LIST/wavl` `data`/`slnt` segments
  in RIFF and RF64, including held-sample silence and bounded streaming reads.
- Expand metadata beyond RIFF INFO where specifications and tests justify it,
  including broadcast, cue, sampler, and embedded metadata chunks.
- [x] Read and write the fixed BWF `bext` fields and coding history, preserving
  opaque versioned bytes and enforcing a 16 MiB chunk bound.
- [x] Read and write typed `cue ` cue-point and `smpl` sample-loop chunks,
  preserving defined fields and bounded sampler-specific data.
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
| RIFF/WAVE and RF64 | Inspect, stream PCM/float/G.711 and mono/stereo IMA ADPCM, read metadata | Write PCM/float/G.711 and mono/stereo IMA ADPCM with non-PCM `fmt ` extensions and `fact` counts; edit RIFF INFO while preserving audio, unknown chunks, and RF64 sizes | IMA ADPCM added; broader codec and corpus coverage remains |
| FLAC | Native FLAC streaming decode with frame CRC, STREAMINFO MD5 verification, and sample seeking for seekable readers; Ogg-FLAC current and tested pre-1.1.1 legacy mapping decode, page CRC, terminal-granule verification, final-frame trimming, and chained links with per-link audio specifications; seekable Ogg-FLAC restart-and-scan sample seeking; Vorbis comments, raw metadata-block preservation, and typed PICTURE/CUESHEET/SEEKTABLE/APPLICATION reads | Native and Ogg-FLAC integer PCM 8/16/24/32-bit encoding; native verbatim/fixed-predictor Rice frames; metadata rewrite and CLI native/Ogg-FLAC output/tag edits | Broader independent Ogg-FLAC source corpus remains open |
| AIFF | Classic AIFF integer PCM at 1–32 valid bits; native text and typed `COMT`/`MARK`/`INST` metadata read/write | Integer PCM writer; FFmpeg encoder interoperability at 1–8 channels | Supported profile; McGill/AFsp corpus and generated cross-decoder coverage pass |
| AIFF-C / AIFC | PCM, float, G.711, Apple IMA4, DWVW 2–32 bits, and G.726 decode at 16/24/32/40 kbit/s when explicit or found in `stocG726`; typed metadata, `AESD`, ID3 compatibility | Integer/float/G.711/IMA4/DWVW/G.726; typed metadata output; CLI `aifc_dwvw` and `aifc_g726 --bitrate 16|24|32|40` | Partial: G.722/G.728/GSM await IPR review; ACE/MACE need complete clean-room algorithm references and independent vectors. Stereo DWVW full-container interoperability and broader corpus work remain open |
| AU / `.au` | Implemented profile: signed PCM 8/16/24/32-bit, IEEE float32/64, G.711 A-law/μ-law, and mono 8 kHz G.721/G.723 3-bit/5-bit ADPCM; bounded raw annotation with UTF-8 COMMENT mapping and audio-preserving metadata rewrite; CLI inspect, conversion, and COMMENT editing. Eight McGill PCM/float/G.711 profiles match FFmpeg sample-for-sample; two malformed McGill files are rejected | Same supported encodings; explicit `au_alaw`, `au_mulaw`, `au_g721`, `au_g723_3`, and `au_g723_5` output | Other AU encoding codes and broader external corpus remain open; Sun confirms low-bit-first G.723 packing; Zemer matches an isolated ITU-T G.191 reconstructed-signal reference with Annex A limiting on 4,096 codewords at each rate. FFmpeg/libsndfile PCM differs on those vectors |
| CAF / raw PCM | Decode supported CAF encodings; raw PCM requires caller-supplied parameters | Encode supported CAF encodings and raw PCM | Lossless / utility |
| WavPack (`.wv`) | Supported for decoding and writing: streaming decode covers lossless PCM, IEEE float32, hybrid `.wvc` restoration, and native DSD modes; lossless integer PCM encoding supports 1–32 channels at 8/16/24/32 bits; hybrid integer PCM encoding writes standard-bitrate `.wv`+`.wvc` pairs; float32 preserves exact bits through uncompressed WVX; raw native DSD encoding supports 1–32 channels; custom rates and APEv2 text editing preserve non-text items | Official suite: 35 supported files decoded, 9 legacy files skipped, and 4 malformed files rejected; DSD has no hybrid `.wvc` mode | Clean-room specification path confirmed; implementation uses Rust `std` and Zemer crates only |
| MP3, AAC | Inspect, decode, metadata, and seek where supported | Encode with explicit profile/quality options | Licensing and build review first |
| DTS Coherent Acoustics (`.dts`) | Inspect and decode DTS core; evaluate DTS-HD and embedded/container streams separately | Evaluate clean-room encoding scope and licensing before promising a writer | Spec-based feasibility review ([ETSI TS 102 114](https://www.etsi.org/deliver/etsi_ts/102100_102199/102114/01.06.01_60/ts_102114v010601p.pdf)) |
| Ogg with Vorbis / Opus | Vorbis has clean-room Float32 decode, chained links, terminal-granule checks, comment rewriting, and CLI inspect. Eight reference-encoder profiles cover 1–8 channels at 8–96 kHz; all match libsndfile PCM oracles over 12,347–30,017 frames; Opus remains planned | Vorbis has experimental fixed-precision and target-bitrate Float32 encoders, plus CLI conversion/comment edits; both encoders model an adaptive 31-point Floor 1 spectral envelope. Standard WAVE masks map to Vorbis channel order. FFmpeg independently decodes synthetic target-rate streams for every channel count from 1–8 and stereo at 192 kHz; ffprobe confirms the declared durations. A 30-second public-domain music excerpt now has near-rate-matched libvorbis comparisons: Zemer measured RMS errors 0.00872/0.00289/0.00149 at 65.8/130.0/194.2 kbit/s versus 0.01950/0.00595/0.00331 for libvorbis at 62.5/125.9/191.3 kbit/s. A 148.2-second worldwide public-domain ambient recording encoded by Xiph libVorbis I 20070622 matches a full FFmpeg-derived 16-bit FLAC oracle; public-domain 3.174-second libVorbis percussion and full 176.587-second CC0 Für Elise recordings add transient and older-version coverage. A 148.2-second public-domain ambient recording also has rate-matched full-stream comparisons at 64/128/192 kbit/s; Zemer measured lower per-channel RMS sample error than FFmpeg 8.1/libvorbis at all three rates. Listening tests and perceptual evaluation remain open; short streams also exceed the nominal average rate because setup headers are large. Implement Opus separately | Keep both codecs separate while sharing `zemer-ogg`; advertise Vorbis only after broader conformance |
| WMA and other legacy formats | Evaluate supported variants and dependencies | Encode only where clean-room scope and licensing permit | Evaluate separately |
| DSF (`.dsf`) | DSF 1.01 raw one-bit/eight-bit DSD decode and encode for mono through 5.1 at 2.8224/5.6448 MHz; reads common ID3v2.2/v2.3/v2.4 text and writes common ID3v2.4 text | CLI inspection, same-format copy with complete bounded ID3 retention, text metadata edits that preserve opaque frames, and DSF/native DSD WavPack transcoding without PCM | Supported for the implemented DSF 1.01 profile; ID3v2.2 compressed tags are ignored as specified, and cross-container artwork transfer is a separate workflow |
| DSDIFF (`.dff`) | Read raw DSD across the full 1–65,535-channel `CHNL` range and compressed DST (up to 64 channels at DSD64/128/256) with optional frame CRC validation, title/artist, general and typed `COMT` comments, channel IDs, and typed markers | Write raw DSD or compressed DST with a clean-room first-order predictor and valid mode-0 bypass fallback; same-format metadata edits preserve audio, typed metadata, and unchanged/unknown chunks | Partial: all three full 300-frame `DST_02_02` vectors and the first 10 frames of every available ISO vector pass bit-exactly; the ISO archive omits `DST_05` vectors. Independent marker/DST interoperability coverage and patent/legal clearance remain open. |
| SACD DSD ISO | Evaluate TOC/area inspection and extraction of readable DSD tracks from user-supplied images | Image creation only if specification-based workflows and tests are practical | Deferred: no authorized normative Scarlet Book layout or test-image rights are established. Public Sony/Philips material is architectural, while SACD also uses content protection. Revisit only with a lawful sector/TOC specification and readable, authorized test images; no DRM circumvention. DST is a separate codec. |

Read and write are separate capabilities: a format may be readable before an
encoder is ready. Formats that depend on restricted, undocumented, or
platform-specific implementations may remain unsupported.

## 4. DSD and optical-disc audio

- DSF 1.01 raw one-bit/eight-bit DSD read/write for the implemented mono-to-5.1
  profile, common ID3 text mapping, audio-preserving metadata edits, and
  DSF/native DSD WavPack CLI transcoding are implemented.
  DSDIFF/DFF reads raw and compressed DST DSD and writes raw or compressed DST
  with mode-0 fallback; comments, markers, and audio-preserving metadata edits
  are supported. All available ISO vector prefixes pass at DSD64/128/256, and
  the three complete 300-frame `DST_02_02` sequences match their raw references.
  ID3v2.2
  compression is intentionally ignored because that tag version defines no
  compression scheme; cross-container DSF artwork transfer remains separate.
- Inspect SACD/DSD ISO navigation, stereo and multichannel areas, track
  boundaries, timing, and metadata only after normative documentation and
  fixture rights are established. The public Philips/Sony overview does not
  define the binary TOC/sector layout, and the Library of Congress notes that
  Scarlet Book access and implementation guidance are unclear. Any future
  extraction must stream readable, unencrypted content and reject protected or
  undocumented paths; see `docs-internal/REFERENCES.md`.
- Expand DST conformance coverage when additional lawfully distributable
  reference vectors become available; the three `DST_05` archive vectors are
  currently missing.
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
- **ISO:** generic ISO 9660 inspection and safe extraction now support
  single-volume primary/Joliet images with non-interleaved files. Rock Ridge,
  multi-volume images, image creation, and other filesystem extensions remain
  planned; SACD DSD ISO is separate and deferred pending public specifications
  and fixture rights.
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
formats and exact read/write support. WAVE/RF64, native FLAC, chained
Ogg-FLAC, DSF 1.01 raw one-bit and eight-bit DSD decoding/encoding for the declared profile,
DSF/native DSD WavPack CLI inspection and transcoding, DSDIFF audio-preserving
metadata edits, WAVE-backed CUE
splitting, CUE creation, and common DSF ID3v2.3/v2.4 text-tag reads/writes are
implemented. Cross-container DSF artwork transfer, remaining codec profiles,
other track types, and
archive/disc workflows remain planned until code and tests land.
