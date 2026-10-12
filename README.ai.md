# Zemer — AI workspace notes

This is a Rust workspace for a clean-room, cross-platform music format library. Target Windows and Linux. Use published specifications and original tests; do not copy proprietary or GPL implementation code.

## Layout

- `crates/pcm`: library crate `zemer-pcm`; shared PCM types and per-representation modules (`u8.rs`, `i16.rs`, `i24.rs`, `i32.rs`, `f32.rs`, `f64.rs`).
- `crates/metadata`: library crate `zemer-metadata`; format-independent metadata tags.
- `crates/core`: library crate `zemer-core`; decoder/encoder contracts, re-exporting metadata and PCM types.
- `crates/flac`: library crate `zemer-flac`; native FLAC streaming decoder (library API, no CLI integration yet).
- `crates/wav`: library crate `wav` (Cargo package `zemer-wav`), the RIFF/WAVE implementation depending on `zemer-core` and `zemer-pcm`.
- `crates/cue`: library crate `cue` (Cargo package `zemer-cue`), CUE parsing/writing types and time conversion.
- `crates/zemer`: Cargo package and executable `zemer`; optional thin CLI. Machine-readable output belongs here, not in format crates.
- `tests/`: workspace-level integration tests; implementation-specific parser unit tests stay in their crate source.
- `ROADMAP.md`: public plan for codecs, conversion, metadata, and disc-image workflows.
- `docs-internal/FORMAT_SUPPORT.md`: local-only implementation support notes; ignored by Git.
- `docs-internal/REFERENCES.md`: local-only research links and regression notes; ignored by Git.
- `docs`: user-facing Sphinx documentation built on Read the Docs, with `conf.py` at the source root.

`README.md` lists only implemented and tested formats and workflows; planned formats belong in `ROADMAP.md`. Native FLAC is currently available through the `zemer-flac` library crate; CLI support is pending.

Keep each new format in a separate crate depending on `zemer-core`. Keep format parsing and metadata mappings inside that format crate, with the common tag model in `zemer-metadata`. Shared sample types and sample-format conversion belong in `zemer-pcm`; decoder/encoder interfaces belong in `zemer-core`. Whole-format conversion should decode to `PcmBuffer` and encode through the target format crate; avoid global registries until callers need them.

Before adding a dependency, record its license and platform/build requirements. Run tests on Linux through `server5`; keep Windows and Linux CI jobs in sync. See `README.md` for current support and public usage.
