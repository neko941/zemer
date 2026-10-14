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

Eight additional Ogg-FLAC fixtures were encoded by FFmpeg 8.1 using
`-c:a flac -compression_level 8 -page_duration 50000 -f ogg`. They contain
only deterministic synthetic samples; tests regenerate each source and compare
every decoded value. Profiles include stereo 16-bit at 48 kHz, 5.1 24-bit at
96 kHz, and 7.1 24-bit at 192 kHz, plus 3-, 4-, 5-, and 7-channel 24-bit at
44.1 kHz. The first five interleaved samples are the signed extrema, `-1`, `0`,
and `1`; subsequent values use deterministic integer formulas. The 5.1 and
7.1 fixtures include packets continued across Ogg pages.

| Fixture | Frames | Channels | Depth | Ogg pages | Fixture SHA-256 | PCM SHA-256 |
|---|---:|---:|---:|---:|---|---|
| `ogg-flac-ffmpeg.oga` | 24,000 | 1 | 24-bit | 8 | `f38a9fdfd7e9530112ebc307d464b7437949915982ba03f69525eaec8f08ee57` | `c53c6281ad5e4ca7e641ef8edf94681ebd7aad848db8073eed8adebb9b38c5e4` |
| `ogg-flac-ffmpeg-stereo-s16.oga` | 8,197 | 2 | 16-bit | 4 | `16e4b5b296fd977c1d0aca7445237b0d98027269192430fc05acc95cd9ccb3bb` | `10af31cad03d5da3a25702bca6adb99aa0001b9f585dd8ab8a0a42ba51a0d9a0` |
| `ogg-flac-ffmpeg-5_1-s24.oga` | 8,197 | 6 | 24-bit | 6 | `60a4576ebe3851e2e0cc2dce2941ac54c5b4ec3dbb1608da0da450feca131fc1` | `2a3e9744d61b18e472c24e12360c142718aaea22712dfabfd8ba68cc81413a8b` |
| `ogg-flac-ffmpeg-7_1-s24-192k.oga` | 8,197 | 8 | 24-bit | 5 | `CC8F71A8F8B123BB301659023AE8EB22F140DC246885381421AE5616B4371C12` | `7EB9567A3543F388E1ED119223C36552EAB78C3C6D49F839D70275AC1C787739` |
| `ogg-flac-ffmpeg-3ch-s24.oga` | 8,197 | 3 | 24-bit | 4 | `def18da34cd595d967de373a4ac604682af6d1fd5fec58f387268ac86a065da3` | `223b8a226cc2b3ace800e8c0e245a6bd0c072db26bd455366bee757320a37302` |
| `ogg-flac-ffmpeg-4ch-s24.oga` | 8,197 | 4 | 24-bit | 4 | `da64dfca27698da4ecfea43bf8dc055f2fe90aeb4a3884b976872f929892d464` | `49d8a568db93ab4296c703043c771329f30dddf2f2cc54f2df9f7dc3a0e25b83` |
| `ogg-flac-ffmpeg-5ch-s24.oga` | 8,197 | 5 | 24-bit | 4 | `a5c244cb2267a9b2d7aa481cf32b5eb948034199c7b4e710d60e0575251221eb` | `6c572514e2d663fd28bc64f5eae70c49420b51f456ee8ab97f1570faf380498b` |
| `ogg-flac-ffmpeg-7ch-s24.oga` | 8,197 | 7 | 24-bit | 6 | `152e9bc19e235783c04c9c9ad17de35bbb308236919b5ca44ad66c427cafd947` | `bd6c21521756627cbf7deef1c3b2a97f4cf046887c5eba15a1e58c9dd2f286d7` |

Each PCM SHA-256 is independently reproduced by FFmpeg decoding. FFmpeg is
used only to create the synthetic interoperability fixtures; no FFmpeg code
or runtime dependency is included.

The ignored test `zemer_ogg_flac_encodings_decode_exactly_with_ffmpeg` checks
Zemer-generated Ogg-FLAC at 8-bit mono/8 kHz, 16-bit stereo/48 kHz, 24-bit
5.1/96 kHz, and 32-bit 7.1/192 kHz. Set `ZEMER_FFMPEG` to the FFmpeg binary
and run `cargo test --test ogg_flac_interop
zemer_ogg_flac_encodings_decode_exactly_with_ffmpeg -- --ignored --exact`.
The test compares the decoded PCM bytes with the original encoder input.

## Ogg Vorbis reference profiles

The ``vorbis/`` fixtures were encoded by libvorbisenc through libsndfile 1.2.2
and decoded to Float32 PCM oracles by libsndfile. They contain deterministic
synthetic tones plus seeded low-level noise, with distinct frequencies per
channel. The normal ``tests/vorbis_interop.rs`` suite compares every decoded
sample against the oracle with a maximum absolute error below 0.002; no
external codec library is used by Zemer or required by CI.
The integration suite also flips one bit in each byte of the independent
44.1-kHz stereo setup header and asserts the parser never panics.

| Sample rate | Channels | Frames | Ogg SHA-256 | PCM oracle SHA-256 |
|---:|---:|---:|---|---|
| 8 kHz | 1 | 12,347 | ``e094c14410706bc0215cdfa8cd7cf9c7ca770b765e73b9578fbefb9c8435e340`` | ``5fbdd31396af7852870741752bdf9d1cc56015e3d76a2e520bd002b8be2b28`` |
| 44.1 kHz | 2 | 12,347 | ``facf7e729f6d4cd50ed283ca4a2924d96c5f4f2c2a5e788f6456101d5943a480`` | ``1bcf85bb5ec4582f794ff97299747cc3afaf0a3ce612de49cf7ad6a91bb04d51`` |
| 32 kHz | 3 | 12,347 | ``b5b3b00e88ec1efbc715c97e83b3271400ba38467a5d70862da4a150200fcdab`` | ``d0e725ead42e93057b67d2f5e716c125721e3de972d803af89298cb46800e77b`` |
| 48 kHz | 6 | 12,347 | ``57c4a0002098353644855ffac9b3d17ac9894266200cbb1bdc9419f567496968`` | ``48a4884fcea56f24a2d26dc326e908bdbb9c336cbdd0601375eaa6927b599b90`` |
| 44.1 kHz | 7 | 12,347 | ``e3b064adca1eaf3aabf125d8e7f3e51682605b5ae4b2103139f9a8c91bda6725`` | ``ebfacac60ebae281b99f6f58ca5b2fd868fd21d1663aa6ae17c79365faff6104`` |
| 96 kHz | 8 | 12,347 | ``7d35aa211a4a90db9efb22f006154b84054d16642a9cd45e8acdb8db74616ee9`` | ``a1cf0d54d2d0001813805cf68b593da8a00c04de9503ac9bbddab40a3270bbb7`` |

The seven-channel input was encoded by FFmpeg 8.1 using its libvorbis encoder;
libsndfile 1.2.2 independently decoded the complete stream to its oracle.

`libsndfile-192000-2.ogg` adds 12,347 stereo frames at 192 kHz, written by
libsndfile 1.2.2's Ogg/Vorbis encoder from distinct 18,137-Hz and 67,021-Hz
tones. FFmpeg 4.2.2's native Vorbis decoder produced the paired Float32 oracle;
CI compares every sample within 0.002. The Ogg SHA-256 is
``c578feec7ee2a7dec9c0ddda90b5f62726833d9e096bf31433b7f21e195a6da2`` and the
oracle SHA-256 is ``30716604a95e0eeaf24781ed2d639640723303722c158d52a7668dc7d7bdbee5``.

Two extra reference streams were encoded with `oggenc` from vorbis-tools
1.4.2/libVorbis 1.3.7. They use distinct channel tones, harmonics, and short
transients, covering channel counts missing from the initial libvorbisenc
profiles. `libsndfile` decoded each stream to a Float32 oracle; normal CI
compares every sample within 0.002.

| Sample rate | Channels | Frames | Ogg SHA-256 | PCM oracle SHA-256 |
|---:|---:|---:|---|---|
| 22.05 kHz | 4 | 18,211 | ``fc4597e4840795902122f8bd46332a31d178952671da977f00d4bc4b1f6044a5`` | ``b15f548bcbb580fd51add5b3bf53959662b6ff09e30ac02fce06d117f687a1fa`` |
| 44.1 kHz | 5 | 30,017 | ``47eb2b7e8c5dee630f302a44abb44ce6f8ed79c6644cfdf848d2cca2a36f2c8d`` | ``2db5c50cc1dccf5b921d6dbe672d54bc413681264905f22d7d462a78311f651d`` |

`wikimedia-cc0-example.ogg` adds a real 44.1-kHz stereo recording licensed
under CC0 1.0 by its author on Wikimedia Commons. The file metadata identifies
Xiph libVorbis I 1.3.3 as its encoder. libsndfile 1.2.2 decoded its 163,328
frames to the paired Float32 oracle; CI compares every sample within 0.002.
Source and licensing: <https://commons.wikimedia.org/wiki/File:Example_sound_file_in_Ogg_Vorbis_format.ogg>.

`wikimedia-cc0-432hz.ogg` adds an 8.08-second 432-Hz stereo recording under
CC0 1.0; its metadata records the older Xiph libVorbis I 20020717 encoder.
libsndfile 1.2.2 decoded all 356,352 frames to the paired Float32 oracle, and
CI compares every sample within 0.002. Source and licensing:
<https://commons.wikimedia.org/wiki/File:432.ogg>.

`rusty-vorbis-0_1_1-44100-2.ogg` was encoded by the independent Rust
[rusty_vorbis 0.1.1](https://crates.io/crates/rusty_vorbis/0.1.1) encoder
(Apache-2.0) from a 12,347-frame excerpt of the CC0 recording above, then
muxed with Zemer's Ogg writer. libsndfile 1.2.2 produced the paired Float32
oracle; CI compares every sample within 0.002. The external encoder is used
only to create this fixture and is not a Zemer dependency.

`oxideav-vorbis-0_0_12-48000-6.ogg` adds six-channel 48-kHz input from the
same independent MIT-licensed encoder. Its 12,347 synthetic frames use a
distinct tone per channel plus a short transient. libsndfile 1.2.2 produced
the paired Float32 oracle; CI compares all six channels within 0.002. Neither
external encoder is a Zemer dependency.

`oxideav-vorbis-0_0_12-44100-2.ogg` was encoded from the same CC0 PCM excerpt
by the independent Rust [oxideav-vorbis 0.0.12](https://crates.io/crates/oxideav-vorbis/0.0.12)
encoder (MIT). libsndfile 1.2.2 decoded the file to the paired Float32 oracle;
CI compares all 12,347 stereo frames within 0.002. The external crate was used
only to create the fixture and is not a Zemer dependency.

| Fixture | Frames | Channels | Ogg SHA-256 | PCM oracle SHA-256 |
|---|---:|---:|---|---|
| `wikimedia-cc0-example.ogg` | 163,328 | 2 | `c001d8980f0b964000a28a99c5cab3809ef65ecf1ba0ae00bc57cb1fc1440754` | `0c8af967233f45f8c4d0de28103f1a524ebe7c23f28a8bd28ecc9a387fd42d14` |
| `wikimedia-cc0-432hz.ogg` | 356,352 | 2 | `eeace8b2c7564aeeca6ac2cbf2d45ad20896bde8fce934ade2f443f1eb667affd` | `5395e86f89f3f01c725520af9f471715f7638f45e08e24be7e59685cf429c97e` |
| `rusty-vorbis-0_1_1-44100-2.ogg` | 12,347 | 2 | `e3c6fb4f2ff40e352d8c1263717a922e51cf665cd0e136e0b2281236d1e37ccd` | `32f9a5d4209f0b0f61c2fbd10bb7af992161fe31c2872ddf84bdba089232a869` |
| `oxideav-vorbis-0_0_12-44100-2.ogg` | 12,347 | 2 | `0ff976d402c8abd8bc3b42f83e008afddb27015f55b35ef6c64dab24bceb0e7d` | `52fedbb8c2cecb0fd1b9cccd5c780096296cec1daeb7fea1fec4eb543216d23e` |
| `oxideav-vorbis-0_0_12-48000-6.ogg` | 12,347 | 6 | `6d65de57e7a89847e5126ed5edb3124b5d75078de75babe9f01123b82a794a40` | `79bc1679226619d83e337d31ba381296a8d1b9d2db7eaf8d6b3974bcd3de59ec` |

Ten optional external [Xiph test vectors](https://people.xiph.org/~xiphmont/test-vectors/vorbis/)
cover Floor 0, older encoder output, and silence. The four `lsp-test*.ogg`
vectors each decoded to 874,606 stereo frames at 44.1 kHz; `rc3-test.ogg` to
934,732 stereo frames; `beta3-test.ogg` to 11,076,156 stereo frames;
`beta4-test.ogg` to 6,952,960 mono frames; `bimS-silence.ogg` to 1,754,165
stereo frames; `rc1-test.ogg` to 6,276,900 stereo frames; and `rc2-test.ogg`
to 7,188,888 stereo frames. All matched separate libsndfile Float32 oracles
within the integration test's 0.002 maximum absolute error on server5. The
vector and oracle files stay outside this repository; run the ignored profile
test with a five-column manifest in `ZEMER_VORBIS_CORPUS`.

Three more Xiph vectors also match full libsndfile PCM oracles: `1.0-test.ogg`
(18,220,356 stereo frames at 44.1 kHz), `1.0.1-test.ogg` (8,199,660 stereo
frames at 44.1 kHz), and `48k-mono.ogg` (515,234 mono frames at 48 kHz).
These vectors and their oracles remain external.

Five additional standalone Xiph vectors match full libsndfile PCM oracles:
`mono.ogg` (13,693,480 mono frames), `sleepzor.ogg` (9,442,944 stereo
frames), `highrate-test.ogg` (12,098,100 stereo frames), `moog.ogg`
(10,546,956 stereo frames), and `rc2-test2.ogg` (7,061,880 stereo frames),
all at 44.1 kHz. `out-of-spec-blocksize.ogg` also matches its complete
libsndfile Float32 oracle at 44.1 kHz, with 9,333,952 stereo frames. The
complete external set now has 19 standalone vectors.
Both official chained vectors also pass independent per-link comparisons:
`chain-test1.ogg` is stereo then mono (2,062,410 and 888,832 frames), while
`chain-test2.ogg` is mono then stereo with the same respective link lengths.
These corpus files and PCM oracles remain external.

The newer Xiph `combustion.ogg` and `make.ogg` vectors put audio packets on
the page that completes the setup header. The [Vorbis I Ogg mapping](https://xiph.org/vorbis/doc/Vorbis_I_spec.html)
requires the first audio packet to start on a fresh page, so these streams are
excluded from exact PCM-oracle comparisons; independent decoders can trim
different leading samples. `unused-mode-test.ogg` reaches an audio packet
whose mode index is outside the stream's setup modes and is rejected by the
decoder.

Do not use Xiph's `test-short.ogg` as an exact PCM oracle. Its page that
completes the setup header also contains audio packets, although the [Vorbis I
Ogg mapping](https://xiph.org/vorbis/doc/Vorbis_I_spec.html) requires the first
audio packet to start on a fresh page. The full
Zemer decode matches the file's terminal granule, while libsndfile and
libvorbisfile discard different leading portions from this nonconforming
stream. Xiph's `test-short2.ogg` is truncated and correctly fails during page
reading.

Do not use Xiph's `chain-test3.ogg` for an exact PCM comparison. Its first
link completes the setup header and begins audio on the same Ogg page, so
independent decoders discard different leading samples; the later 48-kHz mono
link is byte-identical to the separately tested `48k-mono.ogg`. Xiph's
`one-entry-codebook-test.ogg` ends 1,153 bytes before its final Ogg page is
complete, and `singlemap-test.ogg` has no EOS page. The decoder rejects these
incomplete streams at their malformed tails. `unused-mode-test.ogg` reaches an
audio packet whose mode index is outside the setup modes and is also rejected.
The ignored `rejects_malformed_external_xiph_vectors` test checks all five
through EOF using `ZEMER_VORBIS_ONE_ENTRY_TRUNCATED`,
`ZEMER_VORBIS_SINGLEMAP_MISSING_EOS`, `ZEMER_VORBIS_UNUSED_MODE`,
`ZEMER_VORBIS_COMBUSTION_PAGE_SHARING`, and
`ZEMER_VORBIS_MAKE_PAGE_SHARING`; none are bundled. The last two exercise the
same setup-page/audio-page mapping violation described above.

The ignored test `encodes_reference_decoder_channel_profiles` writes the
fixed-profile encoder output for 1–8 channels at 8–96 kHz. The existing
libsndfile checks cover 1, 2, 3, 6, and 8 channels; FFmpeg 4.2.2 independently
decodes the added 4- and 5-channel, 44.1-kHz streams and 7-channel, 48-kHz
stream to exactly 8,193 frames each. To regenerate the files, set
`ZEMER_VORBIS_ENCODED_PROFILE_PREFIX` and run:

```powershell
cargo test -p zemer-tests --test vorbis_interop encodes_reference_decoder_channel_profiles -- --ignored --exact
```

The ignored test `encodes_target_bitrate_profiles_for_external_validation`
writes two-second target-rate streams and their interleaved Float32 inputs for
1–8 channels: mono 32 kHz/32 kbit/s, stereo 44.1 kHz/96 kbit/s, 3-channel
48 kHz/128 kbit/s, 4-channel 48 kHz/192 kbit/s, 5-channel 48 kHz/256 kbit/s,
5.1 48 kHz/256 and 320 kbit/s, 7-channel 48 kHz/384 kbit/s, 7.1 96
kHz/512 kbit/s, and stereo 192 kHz/384 kbit/s. Set
`ZEMER_VORBIS_RATE_PROFILE_PREFIX` to a path prefix and run:

```powershell
cargo test -p zemer-tests --test vorbis_interop encodes_target_bitrate_profiles_for_external_validation -- --ignored --exact
```

FFmpeg independently decodes target-rate output for every channel count from
1 through 8, plus stereo at 192 kHz. FFmpeg 8.1 checks cover the original mono 32 kHz/32 kbit/s,
stereo 44.1 kHz/96 kbit/s, 5.1 48 kHz/256 kbit/s, and 7.1 96 kHz/512 kbit/s
profiles; FFmpeg 4.2.2 checks cover the expanded matrix, including 5.1 at 320
kbit/s. Its `ffprobe` reports exact two-second stream durations; matching
channels by minimum RMS gives a maximum per-channel RMS error below 0.0043
across the first nine profiles. The stereo 192-kHz/384-kbit/s profile is also
decoded with matching channels and exact duration. When exporting
raw PCM with FFmpeg 4.2.2, pass `-t 2`: its raw mono decode otherwise emits
1,536 codec-padding samples past the terminal granule despite reporting a
64,000-frame duration. Its default multichannel PCM output uses WAVE order,
while the encoder input is in Vorbis order; map Vorbis channel indexes to
FFmpeg/WAVE indexes as `[0, 2, 1, 4, 5, 3]` for 5.1 and
`[0, 2, 1, 6, 7, 4, 5, 3]` for 7.1 when comparing samples. On the original
four generated signals, actual two-second file rates were 61.92, 129.39,
300.18, and 550.95 kbit/s; mapped RMS errors were 0.00369, 0.00319, 0.00364,
and 0.00340. Headers account for the elevated average rate on these short
streams; the encoder caps each audio packet using the target rate. These
synthetic results establish decoder interoperability, not perceived audio
quality.

The target-rate encoder now writes a 31-point adaptive Floor 1 spectral
envelope, following the piecewise-linear Floor 1 model in the
[Vorbis I specification](https://www.xiph.org/vorbis/doc/Vorbis_I_spec.html).
A music-source check uses the first 30 seconds of Keith Adler's
[`Promise.ogg` on Wikimedia Commons](https://commons.wikimedia.org/wiki/File:Promise.ogg),
listed there as public domain worldwide. The
source is not bundled; its SHA-1 is
`a9017a0eda837a3308c7ae9e573cbd4b55328036`. Decode the original once to a
Float32 WAV and PCM oracle, encode with Zemer, then decode each output with
FFmpeg 8.1:

```powershell
ffmpeg -i Promise.ogg -t 30 -c:a pcm_f32le Promise-30s.wav
ffmpeg -i Promise.ogg -t 30 -f f32le -acodec pcm_f32le source.f32le
cargo run -p zemer -- convert --format vorbis --bitrate 64 Promise-30s.wav zemer-64.ogg
cargo run -p zemer -- convert --format vorbis --bitrate 128 Promise-30s.wav zemer-128.ogg
cargo run -p zemer -- convert --format vorbis --bitrate 192 Promise-30s.wav zemer-192.ogg
ffmpeg -i zemer-64.ogg -f f32le -acodec pcm_f32le zemer-64.f32le
ffmpeg -i zemer-128.ogg -f f32le -acodec pcm_f32le zemer-128.f32le
ffmpeg -i zemer-192.ogg -f f32le -acodec pcm_f32le zemer-192.f32le
```

For a near-rate-matched libvorbis reference, encode the same Float32 WAV with
FFmpeg requests of 72, 160, and 256 kbit/s, then decode those Ogg files to
interleaved Float32 PCM in the same way:

```powershell
ffmpeg -i Promise-30s.wav -c:a libvorbis -b:a 72k ref-72.ogg
ffmpeg -i Promise-30s.wav -c:a libvorbis -b:a 160k ref-160.ogg
ffmpeg -i Promise-30s.wav -c:a libvorbis -b:a 256k ref-256.ogg
ffmpeg -i ref-72.ogg -f f32le -acodec pcm_f32le ref-72.f32le
ffmpeg -i ref-160.ogg -f f32le -acodec pcm_f32le ref-160.f32le
ffmpeg -i ref-256.ogg -f f32le -acodec pcm_f32le ref-256.f32le
```

Measured rates include Ogg and setup headers; each error is
`sqrt(mean((decoded - source)^2))` across all samples.

| Encoder request | Zemer measured rate / RMS | libvorbis measured rate / RMS |
|---:|---:|---:|
| 64 kbit/s | 65.83 kbit/s / 0.00871945 | 62.50 kbit/s / 0.01949767 (72 kbit/s request) |
| 128 kbit/s | 130.03 kbit/s / 0.00288764 | 125.87 kbit/s / 0.00594757 (160 kbit/s request) |
| 192 kbit/s | 194.22 kbit/s / 0.00149066 | 191.25 kbit/s / 0.00331207 (256 kbit/s request) |

FFmpeg decoded each file to exactly 1,323,000 stereo frames. On this one
ambient track, Zemer had lower sample error at near-matched rates. This is an
objective PCM comparison, not a listening or perceptual-quality test. Zemer's
fixed 12-bit profile measured 1031.59 kbit/s with RMS error 0.00015123 on the
same source, showing the rate profile's quality advantage at a much lower
rate. Two-second synthetic profiles may average above the requested rate
because the setup header is large; the target is a per-audio-packet budget.

The optional ignored test `decodes_external_ogg_flac_against_ffmpeg_pcm_oracle`
checks the unbundled `01_-_dungeon.ogg` sample against signed 16-bit PCM
extracted from `dungeon.wav`. It decoded as 44.1 kHz stereo with 3,730,860
frames and passed on server5 at `ef17955`. The sample's origin and redistribution
rights are undocumented, so the Ogg and WAV files remain external and must not
be bundled. Set `ZEMER_OGG_FLAC_SAMPLE` and `ZEMER_OGG_FLAC_PCM` to local copies
of the Ogg and raw PCM. Source SHA-256:
`be67d426aa7f4f5e44788527a5e1a013fd91a03c57ed1509b384f6eee96cabba`; WAVE SHA-256:
`d63c45932a8021f609b879163d43e6de017ad2ade491ab498c7da4ea5d4527ff`; raw PCM
SHA-256: `1730b770dbde17d07593babbe47b1a8fc0c5d64ac79408495bbdbc62940c3285`.

`ogg-flac-white-noise.oga` is the archived 21 April 2008 revision of [White
noise.ogg on Wikimedia
Commons](https://commons.wikimedia.org/wiki/File:White_noise.ogg). Its file
history describes the revision as a GNU Octave-generated, lossless Ogg-FLAC
encoding; the uploader released the work into the public domain. The [archived
revision](https://upload.wikimedia.org/wikipedia/commons/archive/a/aa/20080421032449%21White_noise.ogg)
is mono, 48 kHz, 16-bit, and 480,000 frames. The test compares every decoded
sample against an FFmpeg 8.1 signed 16-bit little-endian oracle. The Ogg fixture
is 979,572 bytes; the PCM oracle is 960,000 bytes. Source SHA-256:
`265850a669a94f90d60de84dbc1dc9e4afbe6977221b0209f9bc01899dcc7092`; oracle
SHA-256: `ce0460c933fc79c34841a34151209535e71ca0042c505794f52840e44f247e42`.

The optional ignored test `decodes_commons_libflac_1_2_1_dissonance_against_ffmpeg`
checks [Dissonance A220-A440 bandlimited sawtooths](https://commons.wikimedia.org/wiki/File:Dissonance_A220-A440_bandlimited_sawtooths_1.9_MHz_downsampled.oga),
created by Omegatron and licensed CC BY-SA 3.0. The file history identifies
reference libFLAC 1.2.1 as the encoder and says the two-channel signal was
downsampled to 48 kHz. It decodes as 24-bit PCM with 1,440,000 frames. Generate
an s24le oracle with FFmpeg, then set `ZEMER_OGG_FLAC_DISSONANCE_SAMPLE` and
`ZEMER_OGG_FLAC_DISSONANCE_PCM` to the source and oracle paths; neither is
bundled. The audio is unmodified. Source SHA-256:
`80913192f574ffcafbd1e0e380f4b1db7499abe5ec120a4638438dba9e7a309e`; oracle
SHA-256: `a35d1a3b56833384f212f898dffc238874e79a494260d72edac0f9a083eb2512`.

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

`ogg-flac-4m33s.oga` is the public-domain, computer-generated 4′33″ silence
recording from [Wikimedia Commons](https://commons.wikimedia.org/wiki/File:4m33s.oga).
It is a 48 kHz mono 24-bit Ogg-FLAC stream with exactly 13,104,000 silent
frames. The regression decodes every frame and checks both the zero samples
and exact frame count. The source command recorded on Commons is
`sox -n -c1 4m33s.flac trim 0 273`; FFprobe 8.1 confirms the stream profile.
No decoder implementation code is included. SHA-256:
`DC498062CF0DEB4325AD9DA34BED8F8DEC0A1FF7E7042381969ABB1F21BAC520`.

`ogg-flac-nasa-dust-devil.oga` is NASA's public-domain recording of the
Perseverance rover's SuperCam microphone recording a Martian dust devil
([NASA source and WAV download](https://science.nasa.gov/resource/nasas-perseverance-records-a-martian-dust-devil/),
[Ogg-FLAC on Commons](https://commons.wikimedia.org/wiki/File:NASA%27s_Perseverance_Records_a_Martian_Dust_Devil.oga)).
It is 48 kHz stereo 16-bit with 1,295,695 frames. FFmpeg 8.1 decoding of the
Ogg-FLAC and the NASA WAV produces byte-identical PCM; the test checks the
full-stream FNV-1a 64-bit digest `cb02a2047c6f0a19` before and after Ogg
metadata rewriting. Ogg-FLAC fixture SHA-256:
`F5BB21AD7B1738888C51C8B60ECE9AF8E659322723AD296CA34A48F033E641BB`.

`ogg-flac-gymnopedie.oga` is Teknopazzo's 156-second performance of Erik
Satie's *Gymnopédie No. 3*, dedicated under CC0 1.0 on [Wikimedia
Commons](https://commons.wikimedia.org/wiki/File:Gymnop%C3%A9die_no.3.ogg).
It is 22.05 kHz, stereo, signed 16-bit, and 3,442,678 frames. FFmpeg 8.1
decodes it to a 13,770,712-byte s16le oracle (SHA-256
`78b8b419b99688512c5bdf5b1cce1943e53842d707bd28ebddfd4c12f6c16aaf`); the
test checks its full-stream FNV-1a 64-bit digest `1b9480cf175d6580` before and
after Ogg metadata rewriting. Ogg-FLAC fixture SHA-256:
`20a8311eb71e2675de49fa287ef1c0929962838a6dffd76bbf52851c85d70f5d`.

`ogg-flac-david-allen-voice.oga` is a 12.73-second English voice introduction
by David Allen, recorded and uploaded by 1Veertje, and dedicated under CC0 1.0
on [Wikimedia Commons](https://commons.wikimedia.org/wiki/File:David_Allen_-_voice_-_en.ogg).
It is 44.1 kHz, stereo, signed 16-bit, and 561,254 frames. FFmpeg 8.1 decodes
it to a 2,245,016-byte s16le oracle (SHA-256
`cfedee2e6b9ee5c5b2984f19b23b90d34b4d5f5a6b25cc10fb369fb304d43d80`); the
test checks its full-stream FNV-1a 64-bit digest `66d73cd4a324cdb1` before and
after metadata rewriting. Ogg-FLAC fixture SHA-256:
`8b4c7537960adf92542cf7de846de545911f9e2143b6a51857c03cbf47671513`.

`ogg-flac-shami.oga` is the 75.306729-second spoken Wikipedia article about
Shami (Levantine Arabic), published under CC BY-SA 4.0 on [Wikimedia
Commons](https://commons.wikimedia.org/wiki/File:Shami.oga); attribution is to
the authors of the Wikipedia article. The unmodified 5,761,157-byte file matches
Commons SHA-1 `7b82a8cf83c1c2d54804b6a0c628f7ca165ab131`. FFmpeg 8.1 decodes its
48-kHz stereo signed-24 stream to 3,614,723 frames / 21,688,338 bytes (SHA-256
`18d873f3421a98c624ece1b0e6aec39013364c6eced76803c334fa89b65b5409`). The
regression checks the full-stream FNV-1a 64-bit digest
`67f0a56ff1e8f4b7` before and after comment rewriting; rewriting changes only
the comments.

`ogg-flac-toilet-flush.oga` is DrTrumpet's 7.2-second toilet-flush recording,
dedicated under [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/)
on [Wikimedia Commons](https://commons.wikimedia.org/wiki/File:Toilet-flush.ogg).
The Commons metadata identifies reference libFLAC 1.3.0 as the encoder. It is
44.1 kHz stereo signed 16-bit PCM with 317,827 frames. FFmpeg 8.1 decodes it to
a 1,271,308-byte s16le oracle (SHA-256
`2c1c2834a58460e5cbdc4c7d4dd98049abbad1a2b3b78a6e8359ad0075c7abb1`); the
test checks its full-stream FNV-1a 64-bit digest `f534fae5f393257a` before and
after metadata rewriting. Ogg-FLAC fixture SHA-256:
`5887c1f52365f56dc752900c4e7fe5c32132c27ddcfd3783f63354b0ec60f49c`.
The chained-link integration test joins this file to the independent 48 kHz
stereo FFmpeg fixture, rewrites only the first link's title, and verifies both
links' decoded PCM across the 44.1-to-48 kHz transition. It seeks immediately
before and at the link boundary, verifies the active link specifications and
PCM, accepts exact EOF, and rejects seeks beyond EOF.

`ogg-flac-sparse-7_1.oga` is a synthetic 131,072-frame, 48 kHz, 7.1-channel
16-bit stream encoded by FFmpeg 8.1. Exactly one channel carries deterministic
nonzero samples during each 16,384-frame interval while all other channels are
silent. The regression checks all samples against their generating formula,
then seeks immediately before and at each channel transition and near EOF. It
covers the silent-channel run-length seeking case reported in [Xiph FLAC issue
#262](https://github.com/xiph/flac/issues/262). Ogg-FLAC fixture SHA-256:
`a97b48124453436b5a0bce7ee44a20cdcf619b7d5c72089d134c68811143b001`.

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
