# Generated audio fixtures

`ogg-flac-granule-trim.oga` contains a generated 512-frame, 44.1 kHz, mono
16-bit sine wave. The native FLAC stream was encoded with the host's
`libsndfile` 1.x FLAC encoder, then wrapped according to Xiph's Ogg-FLAC 1.0
mapping; the EOS granule was set to 509 and the Ogg page CRC recomputed.

`ogg-flac-granule-oracle.pcm` is the full 512-frame little-endian signed
16-bit PCM output from libsndfile decoding that native FLAC stream. Tests
compare the Ogg-FLAC decoder's 509 output frames with the corresponding oracle
prefix. Both files contain synthetic signal data and are generated for Zemer;
no third-party audio recording or implementation code is included.

Four additional Ogg-FLAC fixtures were encoded by FFmpeg 8.1 using
`-c:a flac -compression_level 8 -page_duration 50000 -f ogg`. They contain
only deterministic synthetic samples; tests regenerate each source and compare
every decoded value. Profiles include stereo 16-bit at 48 kHz, 5.1 24-bit at
96 kHz, and 7.1 24-bit at 192 kHz. The first five interleaved samples are the
signed extrema, `-1`, `0`, and `1`; subsequent values use deterministic integer
formulas. The multichannel fixtures include packets continued across Ogg pages.

| Fixture | Frames | Channels | Depth | Ogg pages | Fixture SHA-256 | PCM SHA-256 |
|---|---:|---:|---:|---:|---|---|
| `ogg-flac-ffmpeg.oga` | 24,000 | 1 | 24-bit | 8 | `f38a9fdfd7e9530112ebc307d464b7437949915982ba03f69525eaec8f08ee57` | `c53c6281ad5e4ca7e641ef8edf94681ebd7aad848db8073eed8adebb9b38c5e4` |
| `ogg-flac-ffmpeg-stereo-s16.oga` | 8,197 | 2 | 16-bit | 4 | `16e4b5b296fd977c1d0aca7445237b0d98027269192430fc05acc95cd9ccb3bb` | `10af31cad03d5da3a25702bca6adb99aa0001b9f585dd8ab8a0a42ba51a0d9a0` |
| `ogg-flac-ffmpeg-5_1-s24.oga` | 8,197 | 6 | 24-bit | 6 | `60a4576ebe3851e2e0cc2dce2941ac54c5b4ec3dbb1608da0da450feca131fc1` | `2a3e9744d61b18e472c24e12360c142718aaea22712dfabfd8ba68cc81413a8b` |
| `ogg-flac-ffmpeg-7_1-s24-192k.oga` | 8,197 | 8 | 24-bit | 5 | `CC8F71A8F8B123BB301659023AE8EB22F140DC246885381421AE5616B4371C12` | `7EB9567A3543F388E1ED119223C36552EAB78C3C6D49F839D70275AC1C787739` |

Each PCM SHA-256 is independently reproduced by FFmpeg decoding. FFmpeg is
used only to create the synthetic interoperability fixtures; no FFmpeg code
or runtime dependency is included.

The optional ignored test `decodes_external_ogg_flac_against_ffmpeg_pcm_oracle`
checks the external FFmpeg sample `01_-_dungeon.ogg` against a separate
FFmpeg-generated signed 16-bit PCM oracle. The sample and oracle are provided
through `ZEMER_OGG_FLAC_SAMPLE` and `ZEMER_OGG_FLAC_PCM`; neither is bundled.

The optional ignored test `decodes_external_commons_ogg_flac_against_libflac_pcm_oracle`
checks the 2011 current-mapping upload of Jono Bacon's `Free Software Song 2`
against PCM decoded by libFLAC 1.4.3. The Commons page records that the Ogg-FLAC
file was made from the artist's FLAC with `flac --ogg -V -8` and is licensed
CC BY-SA 2.0. The test reads the sample and oracle from
`ZEMER_OGG_FLAC_COMMONS_SAMPLE` and `ZEMER_OGG_FLAC_COMMONS_PCM`; neither is
bundled. The source SHA-256 is
`e89f8c5571993a9551c952c19ab22403a709bf89487a2b7421b3d472c8bbc1d8`; the
34,927,200-byte s16le oracle SHA-256 is
`5398cfcb7dc5fc095ae91b8022b7b0c3f6cc863e39e65ee6b4baeba25620ab4c`.

The optional ignored test `decodes_legacy_ogg_flac_against_libflac_pcm_oracle`
checks the historical pre-1.1.1 sample `Yesterday.ogg` against PCM decoded by
libFLAC 1.4; it also checks `When I Grow Up.ogg`, which exercises metadata and
audio packets sharing a page. The files are provided through
`ZEMER_OGG_FLAC_LEGACY_SAMPLE`/`ZEMER_OGG_FLAC_LEGACY_PCM` and
`ZEMER_OGG_FLAC_LEGACY_SECOND_SAMPLE`/`ZEMER_OGG_FLAC_LEGACY_SECOND_PCM`;
neither sample nor oracle is bundled. The test also rewrites both comments,
reads the normalized outputs through EOF, and checks their frame counts;
packet-identity preservation is covered by the crate unit test.

Two additional Ogg-FLAC fixtures were encoded by the system libFLAC 1.4.3
reference encoder. Their PCM is deterministic synthetic data, and integration
tests verify every decoded sample against the source formula rather than a
second binary oracle. The fixtures cover unsigned 8-bit mono at 8 kHz and
signed 32-bit 7.1 at 192 kHz; the latter disables the reference encoder's
streamable-subset restriction. No libFLAC code or dependency is included.

| Fixture | Frames | Channels | Depth | Sample rate | SHA-256 |
|---|---:|---:|---:|---:|---|
| `ogg-flac-reference-u8-mono-8k.oga` | 9,001 | 1 | 8-bit | 8 kHz | `8cc2e49de41f118a11c551571617ccb0597e80bf82ebd763734f73e527ca1096` |
| `ogg-flac-reference-i32-7_1-192k.oga` | 5,003 | 8 | 32-bit | 192 kHz | `d1f4963393464cca3cecf26b12adb6c97e5b95d7c13c6c37a1a11b99a7b8bbad` |

Samples use `u8(frame) = (frame * 73 + 19) mod 256` and
`i32(index) = wrapping_i32(index * 1,000,003 + 0x01234567)`, where `index`
is the interleaved sample index. The fixture tests are `decodes_libflac_reference_u8_mono_at_8khz`
and `decodes_libflac_reference_i32_7_1_at_192khz`.

The optional ignored AIFF test `decodes_external_12_bit_aiff_and_aifc_against_pcm_oracles`
compares two McGill/AFsp 12-bit samples with FFmpeg-generated signed 16-bit PCM
oracles. Both formats decode to the same 23,493-frame stereo signal at 8 kHz.
The external files are not bundled.

The ignored `tests/dst_iso_conformance.rs` tests use official ISO DST bitstreams
and paired raw DSD reference MP4s kept outside the repository. Set
`ZEMER_DST_COMPRESSED_DIR` and `ZEMER_DST_REFERENCE_DIR` to the corresponding
`compressedMp4` and `referencesMp4` directories from the ISO electronic insert.
The default corpus test compares ten frames per vector; a separate ignored test
compares complete sequences.

`aifc-ima4-ffmpeg-8_1.aifc` was encoded by FFmpeg 8.1 from a 0.1-second,
8 kHz synthetic mono tone. `aifc-ima4-ffmpeg-stereo-8_1.aifc` was encoded
from two synthetic tones at 8 kHz. The AIFC COMM frame count is 13 packets;
each Apple IMA4 packet expands to 64 sample frames, so FFmpeg emits 832 PCM frames,
including the final packet's padding. The little-endian PCM oracle in
the matching `.pcm` files is the FFmpeg 8.1 decode of each file. Fixture
SHA-256: mono `5dad81b5eeb6ce894a702d37de36eb1a62e67469ee4017f264e5735a8c902c18`,
stereo `96ab8ba47bcfbd33fb29e34b2bc6d0c8b767ac2929e48eabf1df31daf27caa28`;
PCM SHA-256: mono `e5b640b07c28213fd7af23b3c8fce06994ee358d8e3726d32ff80fd7bec320c2`,
stereo `53b982b4f9d52040dffeb3f36b6e8a99fbd8fa47e5ec0a3ec8ee270ec6c958a5`.
