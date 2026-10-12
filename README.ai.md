# Zemer — AI workspace notes

This is a Rust workspace for a clean-room, cross-platform music format library. Target Windows and Linux. Use published specifications and original tests; do not copy proprietary or GPL implementation code.

## Workspace crates

- [`zemer-core`](crates/core/): shared PCM/DSD decoder and encoder interfaces; re-exports metadata and audio buffer types.
- [`zemer-cue`](crates/cue/) (Rust crate `cue`): CUE sheet types and checked sector/sample-frame conversion.
- [`zemer-dsd`](crates/dsd/): reusable native DSD stream specification and packed octet buffers.
- [`zemer-dsf`](crates/dsf/): DSF 1.01 raw one-bit DSD decoder; ID3 metadata, writing, and CLI integration remain open.
- [`zemer-flac`](crates/flac/) (Rust crate `flac`): native and Ogg-FLAC integer-PCM encoders, native/Ogg-FLAC decoders, native seekable decoding, Vorbis-comment rewriting, and preserved typed metadata blocks. The CLI can encode both mappings. Ogg-FLAC chained-stream decoding and seeking remain open.
- [`zemer-metadata`](crates/metadata/): format-independent tags shared by audio formats.
- [`zemer-ogg`](crates/ogg/): bounded Ogg page/packet reader and writer reused by Ogg-FLAC.
- [`zemer-pcm`](crates/pcm/): reusable PCM sample types and sample-format conversion.
- [`zemer-wav`](crates/wav/) (Rust crate `wav`): RIFF/WAVE library crate.
- [`zemer-wavpack`](crates/wavpack/) (Rust crate `wavpack`): PCM/float32, hybrid `.wvc`, and native DSD decoding; lossless integer PCM encoding for 1–32 channels at 8/16/24/32 bits; standard-bitrate integer PCM hybrid `.wv`+`.wvc` encoding; bit-exact float32 encoding through uncompressed WVX; raw native DSD encoding for 1–32 channels; APEv2 UTF-8 text tags preserve non-text items. The official suite decoded all 35 supported files, skipped 9 legacy files, and rejected all 4 malformed files.
- [`zemer`](crates/zemer/): CLI package and executable for inspection, WAVE/FLAC/WavPack conversion, WAVE/FLAC/WavPack metadata editing, and CUE/WAV workflows.
- [`tests/`](tests/): workspace-level integration tests; codec parser unit tests stay with their format crate.

Ogg-FLAC decoding handles one logical stream and verifies the terminal granule
against decoded sample count, including when STREAMINFO total samples are
unknown, trimming trailing samples from the final decoded block when required.
Independent PCM-oracle validation is still limited to a generated sample; do
not claim broad Ogg-FLAC conformance yet.

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

`README.md` lists only implemented and tested formats and workflows; planned formats belong in `ROADMAP.md`. The CLI can edit WAVE RIFF INFO, native FLAC Vorbis comments, and WavPack APEv2 text tags with `zemer metadata edit`; these rewrite metadata without decoding or re-encoding audio, and WavPack non-text items are retained. `FlacDecoder::metadata_blocks()` retains raw payloads, and FLAC metadata-block accessors expose typed PICTURE/CUESHEET/SEEKTABLE/APPLICATION data.

Keep each new format in a separate crate depending on `zemer-core`. Keep format parsing and metadata mappings inside that format crate, with the common tag model in `zemer-metadata`. Shared sample types and sample-format conversion belong in `zemer-pcm`; decoder/encoder interfaces belong in `zemer-core`. Whole-format conversion should decode to `PcmBuffer` and encode through the target format crate; `transcode` finalizes the encoder after successful copying. Avoid global registries until callers need them.

Before adding a dependency, record its license and platform/build requirements. Run tests on Linux through `server5`; keep Windows and Linux CI jobs in sync. See `README.md` for current support and public usage.
