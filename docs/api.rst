API overview
============

Crate ``zemer-pcm``
-------------------

Format-independent PCM types and sample-format conversion:

* ``AudioSpec`` — sample rate, channel count, sample format, and optional
  speaker-position mask.
* ``Samples`` and ``PcmBuffer`` — interleaved PCM data with validated frame
  counts.
* ``convert_sample_format`` — conversion between integer PCM and IEEE float
  sample formats without resampling or channel remixing.

Crate ``zemer-metadata``
------------------------

* ``Metadata`` — format-independent, case-insensitive text tags with repeated
  values; ``remove()`` removes all values for a key.

Crate ``zemer-core``
--------------------

Streaming decoder and encoder interfaces. It re-exports metadata and PCM types
for convenience and compatibility:

* ``AudioDecoder`` and ``AudioEncoder`` — block-based read and write traits;
  ``AudioEncoder::finish`` finalizes streaming output. ``DsdDecoder`` exposes
  native one-bit audio without passing it through the PCM API.
* ``transcode`` — bounded block copying between matching PCM specifications,
  preserving metadata when the encoder supports it and finalizing the encoder
  after successful copying.

Crate ``zemer-dsd``
-------------------

* ``DsdSpec`` — native one-bit sample rate, channel count, and optional speaker
  mask.
* ``DsdBuffer`` — channel-interleaved packed octets, with DSD bits ordered
  most-significant-bit first within each octet. Each frame holds one octet per
  channel.

The DSD stream interface is in ``zemer-core``; this crate owns its format-neutral
specification and buffer types.

Rust library crate ``cue`` (Cargo package ``zemer-cue``)
--------------------------------------------------------

* CUE track/file models and checked conversion from 75 Hz cue frames to sample
  frame positions.
* UTF-8 and Shift-JIS CUE text parsing for WAVE files and AUDIO tracks.

Rust library crate ``wav`` (Cargo package ``zemer-wav``)
------------------------------------------------------------

* ``WavDecoder<R>`` — seekable RIFF/WAVE and RF64 reader implementing
  ``AudioDecoder``; ``container()`` identifies which one was read.
* ``WavWriter<W>`` — seekable RIFF/WAVE or RF64 writer implementing
  ``AudioEncoder``. Call ``finish`` to finalize its header. Use
  ``WavContainer::Rf64`` with ``new_with_container`` or
  ``new_with_metadata_and_container`` for RF64 output.
* ``write_wav`` — convenience function for writing one complete ``PcmBuffer``.
* ``write_wav_with_metadata`` — write a buffer with supported RIFF INFO tags.
* ``rewrite_metadata`` — edit RIFF INFO tags while preserving encoded audio,
  unknown RIFF/RF64 chunks, and unrecognized INFO subchunks.
* ``WavWriter::new_with_metadata`` — create a streaming writer with title,
  artist, album, date, genre, and comment fields.
* ``G711Encoding`` and ``WavWriter::new_g711`` — explicitly encode signed
  16-bit PCM blocks as G.711 A-law or μ-law WAVE.
* ``WavContainer`` — explicitly choose classic RIFF or RF64 output.

Rust library crate ``flac`` (Cargo package ``zemer-flac``)
----------------------------------------------------------

* ``FlacEncoder<W>`` — seekable streaming encoder for integer PCM 8/16/24/32;
  emits lossless verbatim subframes and patches STREAMINFO totals and MD5 at
  ``finish()``. Floating-point PCM is not supported by this encoder.
* ``FlacDecoder<R>`` — streaming decoder for native FLAC streams; reads
  Vorbis comments into shared ``Metadata`` and exposes retained raw blocks via
  ``metadata_blocks()``.
* ``FlacMetadataBlock`` — preserves the original metadata block type and
  payload; ``data()`` borrows the bytes without interpreting type-specific
  structures. ``picture()`` parses type-6 blocks without copying the image
  bytes; ``cuesheet()`` parses type-5 track and index data.
* ``FlacPicture`` — picture type, media type, UTF-8 description, dimensions,
  color fields, and borrowed image or URI data.
* ``FlacCueSheet``, ``FlacCueTrack``, and ``FlacCueIndex`` — media catalog,
  lead-in, CD-DA flag, track flags, sample offsets, ISRC values, and indices.
* ``FlacSeekTable`` and ``FlacSeekPoint`` — validated seekpoints exposed as a
  borrowed iterator, including placeholder points.
* ``FlacApplication`` — four-byte application ID and borrowed payload.
* ``rewrite_metadata`` — replace or add Vorbis comments while preserving all
  other metadata blocks and copying encoded audio bytes without decoding them.

Rust library crate ``wavpack`` (Cargo package ``zemer-wavpack``)
------------------------------------------------------------------

* ``WavPackBlockHeader`` — reads the fixed 32-byte block header and leaves the
  payload unread.
* ``WavPackMetadataSubBlockHeader`` — reads the bounded metadata sub-block
  header and reports its actual and padded data lengths without allocating or
  reading the payload.
* ``visit_metadata_sub_blocks`` — visits each bounded sub-block in a block
  payload without buffering it; the visitor sees unpadded data, and unread
  bytes are drained before parsing the next header.
* ``WavPackDecoder`` — implements ``AudioDecoder`` for mono and stereo lossless
  integer PCM at 8, 16, 24, and 32 bits, stereo lossless IEEE float32, custom
  sample rates, false stereo, and multichannel lossless PCM. It also decodes
  mono/stereo hybrid-lossy PCM16 and float32, hybrid-lossless PCM16/24/32 and
  float32 with paired ``.wvc`` files, and shaped/cross-channel hybrid PCM16. Tests
  cover 3–8 channel PCM16 groups, 3-channel hybrid PCM16, and 5.1 PCM24/32. Pair
  correction streams with ``WavPackDecoder::new_with_correction``. WavPack v5
  block, decoded-sample, and integer/float WVX checksums are verified.
* ``WavPackDsdDecoder`` — implements ``DsdDecoder`` for native DSD direct, fast,
  and adaptive profiles, verifying each decoded block checksum. Oracle tests
  cover all three profiles in stereo and adaptive DSD in 5.1.

The multichannel block-group fixtures cover 3–8 channel PCM16. WavPack DSD is
always lossless and has no hybrid ``.wvc`` correction mode.
Hybrid-lossless restoration has conformance coverage for stereo PCM16/24/32
and float32, including shaped and cross-channel PCM16 profiles.
The float32 conformance fixture checks exact IEEE-754 bit patterns including
signed zero, subnormals, infinities, and NaNs. The crate uses only Rust's
standard library and ``zemer-core``; no external codec implementation is linked.

Generate complete local Rust API documentation from the repository root:

.. code-block:: console

   cargo doc --workspace --no-deps --open
