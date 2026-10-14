# Zemer — AI workspace notes

This is a Rust workspace for a clean-room, cross-platform music format library. Target Windows and Linux. Use published specifications and original tests; do not copy proprietary or GPL implementation code.

## Workspace crates

- [`zemer-core`](crates/core/): shared PCM/DSD decoder and encoder interfaces; re-exports metadata and audio buffer types.
- [`zemer-cue`](crates/cue/) (Rust crate `cue`): clean-room WAVE/AUDIO CUE parser, writer, and checked sector/sample-frame conversion; UTF-8/Shift-JIS input is capped at 16 MiB.
- [`zemer-dsd`](crates/dsd/): reusable native DSD stream specification and packed octet buffers.
- [`zemer-dst`](crates/dst/): reusable clean-room DST frame decoder and first-order encoder for DSD64/128/256 profiles.
- [`zemer-dsdiff`](crates/dsdiff/) (Rust crate `dsdiff`): streams raw DSD across the full 1–65,535-channel DSDIFF `CHNL` range and compressed DST (up to 64 channels at DSD64/128/256, including optional frame CRC checks), and writes raw DSD or DST frames compressed with a clean-room first-order predictor when useful, falling back to valid processing-mode-0 bypass frames. CLI output selects compressed DST with `--format dsdiff_dst`. It supports title, artist, general/typed COMT comments, channel IDs, and typed DIIN/MARK records. Same-format CLI conversion retains typed comments and markers. Metadata edits preserve audio and unchanged/unknown chunks. All three complete 300-frame `DST_02_02` vectors pass bit-exactly; the first 10 frames of every available ISO DST vector also pass.
- [`zemer-dsf`](crates/dsf/): DSF 1.01 raw DSD decoder/encoder, supporting both one-bit and eight-bit storage order; reads common uncompressed ID3v2.2/v2.3/v2.4 text tags and writes common ID3v2.4 text tags. Its format-specific API and DSF-to-DSF CLI conversion preserve complete bounded raw ID3 tags; CLI text edits retain artwork/opaque frames and copy DSD bytes unchanged, including tags that use tag-level unsynchronization or extended headers. Cross-format DSD conversion carries common text only. ID3v2.2 tags with the reserved compression flag are ignored as required by that tag version.
- [`zemer-flac`](crates/flac/) (Rust crate `flac`): native and Ogg-FLAC integer-PCM encoders, native/Ogg-FLAC decoders, native seekable decoding, Ogg-FLAC restart-and-scan seeking, native and Ogg-FLAC Vorbis-comment rewriting, and preserved typed metadata blocks. Ogg decoding accepts the current mapping and the tested pre-1.1.1 legacy packet layout; encoding and metadata edits write the current mapping. Ogg metadata edits preserve encoded audio packets and update only the first chained link. Chained Ogg-FLAC links may use different audio specifications; `spec()` and each returned PCM block report the active link's specification. CLI inspect and convert only summarize chains whose links share one audio specification.
- [`zemer-g711`](crates/g711/) (Rust crate `g711`): shared G.711 A-law/μ-law sample encoder and decoder used by WAVE and AIFF-C.
- [`zemer-metadata`](crates/metadata/): format-independent tags shared by audio formats.
- [`zemer-ogg`](crates/ogg/): bounded Ogg page/packet reader and writer reused by Ogg-FLAC.
- [`zemer-vorbis`](crates/vorbis/): clean-room Vorbis I Float32 decoder and experimental encoders, including Floor 0/1, residue 0/1/2, inverse coupling, inverse MDCT, windowing, overlap-add, sequential chained links with differing specifications, terminal-granule trimming/validation, and first-link comment rewriting that preserves encoded packets. Both encoder profiles use an adaptive 31-point Floor 1 spectral envelope; the fixed profile uses 8192-sample blocks, omits 16-bin residue partitions that quantize entirely to zero, and has a scalar residue codebook configurable from 8 to 16 bits (12-bit default). An experimental target-bitrate constructor assigns a per-audio-packet budget, accounting for Ogg page overhead, and greedily allocates quantizer precision by unweighted spectral squared-error reduction. Its effective minimum depends on sample rate and channel count because mandatory packet overhead varies by profile; targets that cannot fit are rejected. FFmpeg independently decodes two-second target-rate profiles at 1–8 channels plus 192-kHz stereo, including 5.1 at 256 and 320 kbit/s; ffprobe confirms exact stream durations, and channel-order-matched PCM has maximum per-channel RMS error below 0.0043. The fixed encoder also round-trips a one-frame 255-channel stream internally; this is a boundary check, not high-channel interoperability coverage. For the mono profile, FFmpeg 4.2.2 raw PCM export emits 1,536 padding samples unless clipped to the stream duration. On a 30-second public-domain music excerpt, Zemer's target-rate output at 65.8/130.0/194.2 kbit/s measured RMS errors of 0.00872/0.00289/0.00149 against the decoded input; near-rate-matched FFmpeg/libvorbis outputs measured 0.01950/0.00595/0.00331. These are sample-error measurements, not perceptual listening results. libsndfile independently verifies the fixed encoder's five quantizer precisions and mono, stereo, 3-channel, 5.1, and 7.1 outputs. In an independent comparison, fixed-profile zero-partition omission reduced a 44.1-kHz stereo sample from 73,413 to 35,753 bytes (51.3%) with bit-identical libsndfile-decoded Float32 PCM. The decoder matches independent PCM oracles for libvorbisenc streams at mono 8 kHz, stereo 44.1 kHz, three-channel 32 kHz, 5.1 48 kHz, and 7.1 96 kHz (12,347 frames per profile; maximum absolute error below 0.002); a libsndfile 1.2.2 192-kHz stereo stream decoded by FFmpeg 4.2.2 (12,347 frames); and two further libVorbis 1.3.7 `oggenc` streams with 4 channels at 22.05 kHz and 5 channels at 44.1 kHz (18,211 and 30,017 frames), matched to libsndfile oracles; plus 19 standalone external Xiph vectors: `1.0-test.ogg`, `1.0.1-test.ogg`, `48k-mono.ogg`, `rc3-test.ogg`, `beta3-test.ogg`, `beta4-test.ogg`, `bimS-silence.ogg`, `rc1-test.ogg`, `rc2-test.ogg`, `mono.ogg`, `sleepzor.ogg`, `highrate-test.ogg`, `moog.ogg`, `rc2-test2.ogg`, `out-of-spec-blocksize.ogg`, and four Floor 0 LSP vectors. Both official Xiph chained vectors also pass per-link PCM comparisons while changing channel count between links. Xiph `test-short.ogg`, the first link of `chain-test3.ogg`, and `combustion.ogg` and `make.ogg` are excluded as exact PCM oracles because setup and audio packets share a page contrary to the mapping's fresh-page rule; independent decoders discard different leading portions. `one-entry-codebook-test.ogg` is truncated in its final page, `singlemap-test.ogg` lacks an EOS page, and `unused-mode-test.ogg` reaches an out-of-range audio-mode index and is rejected. The CLI can inspect, convert to Vorbis with optional `--bitrate` in kbit/s, and edit Ogg Vorbis comments; conversion maps recognized WAVE speaker masks into Vorbis channel order and rejects unrecognized masks. Malformed Ogg truncation and packet-concealment checks pass; perceptual evaluation and wider bitstream conformance remain open, so the format is not advertised as supported.
- [`zemer-pcm`](crates/pcm/): reusable PCM sample types and sample-format conversion.
- [`zemer-wav`](crates/wav/) (Rust crate `wav`): RIFF/WAVE library crate.
- [`zemer-aiff`](crates/aiff/) (Rust crate `aiff`): AIFF/AIFC streaming PCM/float/G.711/IMA4 library crate. Reads AIFF and AIFC `NONE`/`twos`/`sowt` integer PCM, AIFC `fl32`/`fl64`, A-law/μ-law codes, Apple IMA4 packets, and typed `COMT`/`MARK` records; writes AIFF integer PCM, AIFC integer PCM/float/G.711/IMA4, and typed `COMT`/`MARK` records; rewrites NAME/AUTH/ANNO tags without changing audio bytes. AIFF-to-AIFF/AIFC CLI conversion preserves MARK records.
- [`zemer-wavpack`](crates/wavpack/) (Rust crate `wavpack`): PCM/float32, hybrid `.wvc`, and native DSD decoding; lossless integer PCM encoding for 1–32 channels at 8/16/24/32 bits; standard-bitrate integer PCM hybrid `.wv`+`.wvc` encoding; bit-exact float32 encoding through uncompressed WVX; raw native DSD encoding for 1–32 channels; APEv2 UTF-8 text tags preserve non-text items. The official suite decoded all 35 supported files, skipped 9 legacy files, and rejected all 4 malformed files.
- [`zemer`](crates/zemer/): CLI package and executable for inspection, WAVE/AIFF/FLAC/Ogg Vorbis/WavPack conversion, explicit WAVE/AIFC G.711 and AIFC IMA4 output, DSF/DSDIFF/native DSD WavPack transcoding, WAVE/AIFF/DSF/DSDIFF/native, Ogg-FLAC/Ogg Vorbis, and WavPack metadata editing, and CUE/WAV workflows.
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
current-mapping audio sources and two archived Commons samples were compared
with FFmpeg/libFLAC PCM oracles; two pre-1.1.1 legacy samples were compared
with libFLAC PCM oracles. These optional ignored tests use external temporary
files. A broad independent Ogg-FLAC source corpus remains untested; do not
claim broad corpus conformance.

AIFF and AIFC PCM/float/G.711/IMA4 support is in `crates/aiff/`. The optional
ignored `tests/aiff_interop.rs` test compares the McGill/AFsp 12-bit AIFF and
AIFC samples with independent FFmpeg PCM oracles; the sample files and oracles
are temporary inputs, not bundled fixtures.

The WAVE reader handles top-level `data` and ordered `LIST/wavl` `data`/`slnt`
segments in RIFF and RF64. Silence segments hold the previous interleaved frame;
initial silence uses neutral sample codes. The reader limits files to one
million retained audio segments and supported INFO metadata to 16 MiB total,
65,536 values, and 1 MiB per value. The writer still emits one ordinary
contiguous `data` chunk.

## Other repository paths

- `crates/pcm/src/`: shared PCM types and per-representation modules (`u8.rs`, `i16.rs`, `i24.rs`, `i32.rs`, `f32.rs`, `f64.rs`).
- `ROADMAP.md`: public plan for codecs, conversion, metadata, and disc-image workflows.
- `docs-internal/FORMAT_SUPPORT.md`: local-only implementation support notes; ignored by Git.
- `docs-internal/REFERENCES.md`: local-only research links and regression notes; ignored by Git.
- `docs`: user-facing Sphinx documentation built on Read the Docs, with `conf.py` at the source root.

`README.md` lists only implemented and tested formats and workflows; planned formats belong in `ROADMAP.md`. The CLI can edit WAVE RIFF INFO, AIFF/AIFC NAME/AUTH/ANNO text chunks, DSF ID3 text, DSDIFF text/comments, native or Ogg-FLAC Vorbis comments, and WavPack APEv2 text tags with `zemer metadata edit`; these rewrite metadata without decoding or re-encoding audio, preserving unknown AIFF/DSDIFF chunks and WavPack non-text items. Ogg-FLAC metadata edits preserve encoded FLAC packets and update the first chained link only. `FlacDecoder::metadata_blocks()` retains raw payloads, and FLAC metadata-block accessors expose typed PICTURE/CUESHEET/SEEKTABLE/APPLICATION data.

Keep each new format in a separate crate depending on `zemer-core`. Keep format parsing and metadata mappings inside that format crate, with the common tag model in `zemer-metadata`. Shared sample types and sample-format conversion belong in `zemer-pcm`; decoder/encoder interfaces belong in `zemer-core`. Whole-format conversion should decode to `PcmBuffer` and encode through the target format crate; `transcode` finalizes the encoder after successful copying. Avoid global registries until callers need them.

CLI conversion preserves a floating-point input for formats that support it. Float input converted to native FLAC, Ogg-FLAC, or AIFF uses 24-bit integer PCM; AIFC can preserve IEEE float samples.

The Ogg reader exposes the EOS granule even when the terminal page has no packets. Vorbis holds the final overlap-add chunk until that granule is known, then trims it; comment rewriting moves the terminal granule onto the final packet page. Comment packets may continue across Ogg pages; rewriting a 70 KiB lyrics field preserves all encoded audio packets. CLI metadata edits retain unmodified repeated comments, including `METADATA_BLOCK_PICTURE`, while replacing or removing only requested keys.

Vorbis header validation requires the setup packet to end its Ogg page, so the first audio packet begins on a fresh page as required by the mapping. A synthetic page-sharing regression verifies that non-compliant streams are rejected.

Vorbis decoding also uses the granule on the page completing the second audio packet to trim pre-zero samples or account for a positive start position. Non-zero offsets require that packet to end its page, so the third packet begins fresh; terminal granules remain relative to the starting position, and standard pages with no granule do not change it.

The public Vorbis decoder accepts short reads and retries interrupted reads from its input source; an end-to-end regression compares its PCM with the ordinary reader path.

The Vorbis integration tests flip every bit in an independent real setup header and assert parsing never panics. A generated Vorbis stream is also rejected at every truncation point in its identification page; later pages have representative truncation checks.

Checked-in Vorbis input fixtures include Xiph/libVorbis and two independent Rust encoder outputs. `oxideav-vorbis 0.0.12` coverage includes stereo 44.1-kHz music and a synthetic six-channel 48-kHz transient profile, each paired with a full libsndfile 1.2.2 Float32 oracle; both external encoders are test-fixture sources only and are not Zemer dependencies.

For later Ogg pages, the decoder checks each owned granule against the decoded PCM endpoint when a following packet arrives. It defers the most recent page's check so a final audio packet followed by a nil EOS page can still use the EOS granule to trim its overlap tail.

Before adding a dependency, record its license and platform/build requirements. Run tests on Linux through `server5`; keep Windows and Linux CI jobs in sync. See `README.md` for current support and public usage.
