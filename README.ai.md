# Zemer — AI workspace notes

This is a Rust workspace for a clean-room, cross-platform music format library. Target Windows and Linux. Use published specifications and original tests; do not copy proprietary or GPL implementation code.

## Layout

- `crates/zemer-core`: format-independent PCM data, metadata, and decoder/encoder contracts.
- `crates/wav`: library crate `wav` (Cargo package `zemer-wav`), the RIFF/WAVE implementation depending on `zemer-core`.
- `crates/cue`: library crate `cue` (Cargo package `zemer-cue`), CUE parsing/writing types and time conversion.
- `crates/zemer-cli`: optional thin CLI; machine-readable output belongs here, not in format crates.
- `tests/`: workspace-level integration tests; implementation-specific parser unit tests stay in their crate source.
- `ROADMAP.md`: public plan for codecs, conversion, metadata, and disc-image workflows.
- `docs-internal/FORMAT_SUPPORT.md`: local-only implementation support notes; ignored by Git.
- `docs-internal/REFERENCES.md`: local-only research links and regression notes; ignored by Git.
- `docs`: user-facing Sphinx documentation built on Read the Docs, with `conf.py` at the source root.

`README.md` lists only implemented and tested formats and workflows; planned formats belong in `ROADMAP.md`.

Keep each new format in a separate crate depending on `zemer-core`. Keep format parsing and metadata mappings inside that format crate. Shared sample and error types belong in `zemer-core`; avoid global registries until callers need them. Use the core sample-format conversion for sample representation changes; conversion must preserve speaker masks and supported metadata or return an explicit error.

Before adding a dependency, record its license and platform/build requirements. Run tests on Linux through `server5`; keep Windows and Linux CI jobs in sync. See `README.md` for current support and public usage.
