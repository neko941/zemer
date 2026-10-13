# Zemer — AI workspace notes

This is a Rust workspace for a clean-room, cross-platform music format library. Target Windows and Linux. Use published specifications and original tests; do not copy proprietary or GPL implementation code.

## Workspace crates

- [`zemer-core`](crates/core/): shared PCM/DSD decoder and encoder interfaces; re-exports metadata and audio buffer types.
- [`zemer-cue`](crates/cue/) (Rust crate `cue`): CUE sheet types and checked sector/sample-frame conversion.
- [`zemer-dsd`](crates/dsd/): reusable native DSD stream specification and packed octet buffers.
- [`zemer-dst`](crates/dst/): reusable clean-room DST frame decoder and first-order encoder for DSD64/128/256 profiles.
- [`zemer-dsdiff`](crates/dsdiff/) (Rust crate `dsdiff`): streams raw DSD across the full 1–65,535-channel DSDIFF `CHNL` range and compressed DST (up to 64 channels at DSD64/128/256, including optional frame CRC checks), and writes raw DSD or DST frames compressed with a clean-room first-order predictor when useful, falling back to valid processing-mode-0 bypass frames. CLI output selects compressed DST with `--format dsdiff_dst`. It supports title, artist, general/typed COMT comments, channel IDs, and typed DIIN/MARK records. Same-format CLI conversion retains typed comments and markers. Metadata edits preserve audio and unchanged/unknown chunks. All three complete 300-frame `DST_02_02` vectors pass bit-exactly; the first 10 frames of every available ISO DST vector also pass.
- [`zemer-dsf`](crates/dsf/): DSF 1.01 raw DSD decoder/encoder, supporting both one-bit and eight-bit storage order; reads common uncompressed ID3v2.2/v2.3/v2.4 text tags and writes common ID3v2.4 text tags. Its format-specific API and DSF-to-DSF CLI conversion preserve complete bounded raw ID3 tags; CLI text edits retain artwork/opaque frames and copy DSD bytes unchanged, including tags that use tag-level unsynchronization or extended headers. Cross-format DSD conversion carries common text only. ID3v2.2 tags with the reserved compression flag are ignored as required by that tag version.
- [`zemer-flac`](crates/flac/) (Rust crate `flac`): native and Ogg-FLAC integer-PCM encoders, native/Ogg-FLAC decoders, native seekable decoding, Ogg-FLAC restart-and-scan seeking, native and Ogg-FLAC Vorbis-comment rewriting, and preserved typed metadata blocks. Ogg decoding accepts the current mapping and the tested pre-1.1.1 legacy packet layout; encoding and metadata edits write the current mapping. Ogg metadata edits preserve encoded audio packets and update only the first chained link. Chained Ogg-FLAC links may use different audio specifications; `spec()` and each returned PCM block report the active link's specification. CLI inspect and convert only summarize chains whose links share one audio specification.
- [`zemer-g711`](crates/g711/) (Rust crate `g711`): shared G.711 A-law/μ-law sample encoder and decoder used by WAVE and AIFF-C.
- [`zemer-metadata`](crates/metadata/): format-independent tags shared by audio formats.
- [`zemer-ogg`](crates/ogg/): bounded Ogg page/packet reader and writer reused by Ogg-FLAC.
- [`zemer-vorbis`](crates/vorbis/): clean-room Vorbis I crate in development; it parses identification, comment, and setup headers, but does not yet decode audio to PCM or encode audio.
- [`zemer-pcm`](crates/pcm/): reusable PCM sample types and sample-format conversion.
- [`zemer-wav`](crates/wav/) (Rust crate `wav`): RIFF/WAVE library crate.
- [`zemer-aiff`](crates/aiff/) (Rust crate `aiff`): AIFF/AIFC streaming PCM/float/G.711/IMA4 library crate. Reads AIFF and AIFC `NONE`/`twos`/`sowt` integer PCM, AIFC `fl32`/`fl64`, A-law/μ-law codes, and Apple IMA4 packets; writes AIFF integer PCM, AIFC integer PCM/float/G.711/IMA4, and typed `COMT` records; rewrites NAME/AUTH/ANNO tags without changing audio bytes.
- [`zemer-wavpack`](crates/wavpack/) (Rust crate `wavpack`): PCM/float32, hybrid `.wvc`, and native DSD decoding; lossless integer PCM encoding for 1–32 channels at 8/16/24/32 bits; standard-bitrate integer PCM hybrid `.wv`+`.wvc` encoding; bit-exact float32 encoding through uncompressed WVX; raw native DSD encoding for 1–32 channels; APEv2 UTF-8 text tags preserve non-text items. The official suite decoded all 35 supported files, skipped 9 legacy files, and rejected all 4 malformed files.
- [`zemer`](crates/zemer/): CLI package and executable for inspection, WAVE/AIFF/FLAC/WavPack conversion, explicit WAVE/AIFC G.711 and AIFC IMA4 output, DSF/DSDIFF/native DSD WavPack transcoding, WAVE/AIFF/DSF/DSDIFF/native and Ogg-FLAC/WavPack metadata editing, and CUE/WAV workflows.
- [`tests/`](tests/): workspace-level integration tests; codec parser unit tests stay with their format crate.

Ogg-FLAC decoding verifies each link's terminal granule against its decoded
sample count plus the initial granule offset, including when STREAMINFO total
samples are unknown, and trims trailing samples from the final decoded block
when required. Chained links may have different audio specifications; metadata
comes from the first link.
Seekable sources can restart at their initial Ogg offset and scan forward to a
target sample; this path is linear. The optional official CC0 FLAC testbench
also round-trips accepted cases through the Ogg-FLAC encoder and decoder.
Independent FFmpeg-generated Ogg-FLAC PCM oracles cover four synthetic
profiles, from mono 24-bit through 7.1 24-bit at 192 kHz, including large
packets continued across Ogg pages. Two synthetic Ogg-FLAC files from the
libFLAC 1.4.3 reference encoder add 8-bit mono and 32-bit 7.1 coverage. The
official CC0 native-FLAC corpus also
round-trips through Zemer's Ogg-FLAC encoder and decoder. Six external
current-mapping samples were compared with FFmpeg/libFLAC PCM oracles, and two
pre-1.1.1 legacy samples were compared with libFLAC PCM oracles. These optional
ignored tests use external temporary files. A broad independent Ogg-FLAC
source corpus remains untested; do not claim broad corpus conformance.

AIFF and AIFC PCM/float/G.711/IMA4 support is in `crates/aiff/`. The optional
ignored `tests/aiff_interop.rs` test compares the McGill/AFsp 12-bit AIFF and
AIFC samples with independent FFmpeg PCM oracles; the sample files and oracles
are temporary inputs, not bundled fixtures.

The WAVE reader handles top-level `data` and ordered `LIST/wavl` `data`/`slnt`
segments in RIFF and RF64. Silence segments hold the previous interleaved frame;
initial silence uses neutral sample codes. The reader limits files to one
million retained audio segments. The writer still emits one ordinary contiguous
`data` chunk.

## Other repository paths

- `crates/pcm/src/`: shared PCM types and per-representation modules (`u8.rs`, `i16.rs`, `i24.rs`, `i32.rs`, `f32.rs`, `f64.rs`).
- `ROADMAP.md`: public plan for codecs, conversion, metadata, and disc-image workflows.
- `docs-internal/FORMAT_SUPPORT.md`: local-only implementation support notes; ignored by Git.
- `docs-internal/REFERENCES.md`: local-only research links and regression notes; ignored by Git.
- `docs`: user-facing Sphinx documentation built on Read the Docs, with `conf.py` at the source root.

`README.md` lists only implemented and tested formats and workflows; planned formats belong in `ROADMAP.md`. The CLI can edit WAVE RIFF INFO, AIFF/AIFC NAME/AUTH/ANNO text chunks, DSF ID3 text, DSDIFF text/comments, native or Ogg-FLAC Vorbis comments, and WavPack APEv2 text tags with `zemer metadata edit`; these rewrite metadata without decoding or re-encoding audio, preserving unknown AIFF/DSDIFF chunks and WavPack non-text items. Ogg-FLAC metadata edits preserve encoded FLAC packets and update the first chained link only. `FlacDecoder::metadata_blocks()` retains raw payloads, and FLAC metadata-block accessors expose typed PICTURE/CUESHEET/SEEKTABLE/APPLICATION data.

Keep each new format in a separate crate depending on `zemer-core`. Keep format parsing and metadata mappings inside that format crate, with the common tag model in `zemer-metadata`. Shared sample types and sample-format conversion belong in `zemer-pcm`; decoder/encoder interfaces belong in `zemer-core`. Whole-format conversion should decode to `PcmBuffer` and encode through the target format crate; `transcode` finalizes the encoder after successful copying. Avoid global registries until callers need them.

Before adding a dependency, record its license and platform/build requirements. Run tests on Linux through `server5`; keep Windows and Linux CI jobs in sync. See `README.md` for current support and public usage.
