# Zemer — AI workspace notes

This is a Rust workspace for a clean-room, cross-platform music format library. Target Windows and Linux. Use published specifications and original tests; do not copy proprietary or GPL implementation code.

## Workspace crates

- [`zemer-core`](crates/core/): shared decoder/encoder interfaces; re-exports metadata and PCM types for convenience.
- [`zemer-cue`](crates/cue/) (Rust crate `cue`): CUE sheet types and checked sector/sample-frame conversion.
- [`zemer-flac`](crates/flac/) (Rust crate `flac`): native FLAC streaming decoder with frame CRC and STREAMINFO MD5 verification after full consumption, and a Vorbis-comment rewriter with preserved raw metadata blocks and typed PICTURE/CUESHEET/SEEKTABLE/APPLICATION access.
- [`zemer-metadata`](crates/metadata/): format-independent tags shared by audio formats.
- [`zemer-pcm`](crates/pcm/): reusable PCM sample types and sample-format conversion.
- [`zemer-wav`](crates/wav/) (Rust crate `wav`): RIFF/WAVE library crate.
- [`zemer-wavpack`](crates/wavpack/) (Rust crate `wavpack`): bounded block inspection with WavPack v5 block-checksum verification, mono/stereo lossless integer PCM (8/16/24/32-bit), stereo lossless IEEE float32, 5.1 and 7.1 lossless PCM16 block groups, mono/stereo unshaped hybrid-lossy PCM16, tested stereo noise-shaped hybrid-lossy PCM16, and tested stereo PCM16 hybrid-lossless restoration with `.wvc` in flat and shaped modes; cross-channel `.wvc` correction, other multichannel layouts, decoded-sample CRC verification, and DSD remain incomplete.
- [`zemer`](crates/zemer/): CLI package and executable for inspection, conversion, WAVE/FLAC metadata editing, and CUE/WAV workflows.
- [`tests/`](tests/): workspace-level integration tests; codec parser unit tests stay with their format crate.

## Other repository paths

- `crates/pcm/src/`: shared PCM types and per-representation modules (`u8.rs`, `i16.rs`, `i24.rs`, `i32.rs`, `f32.rs`, `f64.rs`).
- `ROADMAP.md`: public plan for codecs, conversion, metadata, and disc-image workflows.
- `docs-internal/FORMAT_SUPPORT.md`: local-only implementation support notes; ignored by Git.
- `docs-internal/REFERENCES.md`: local-only research links and regression notes; ignored by Git.
- `docs`: user-facing Sphinx documentation built on Read the Docs, with `conf.py` at the source root.

`README.md` lists only implemented and tested formats and workflows; planned formats belong in `ROADMAP.md`. The CLI can edit WAVE RIFF INFO and native FLAC Vorbis comments with `zemer metadata edit`; both rewrite metadata without decoding or re-encoding audio. `FlacDecoder::metadata_blocks()` retains raw payloads, and FLAC metadata-block accessors expose typed PICTURE/CUESHEET/SEEKTABLE/APPLICATION data.

Keep each new format in a separate crate depending on `zemer-core`. Keep format parsing and metadata mappings inside that format crate, with the common tag model in `zemer-metadata`. Shared sample types and sample-format conversion belong in `zemer-pcm`; decoder/encoder interfaces belong in `zemer-core`. Whole-format conversion should decode to `PcmBuffer` and encode through the target format crate; `transcode` finalizes the encoder after successful copying. Avoid global registries until callers need them.

Before adding a dependency, record its license and platform/build requirements. Run tests on Linux through `server5`; keep Windows and Linux CI jobs in sync. See `README.md` for current support and public usage.
