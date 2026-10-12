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
* ``apev2::ApeTag`` — bounded APEv2 footer parsing/writing, UTF-8 text mapping,
  and text replacement that retains non-text items.

Crate ``zemer-ogg``
-------------------

* ``OggPacketReader<R>`` — validates Ogg pages and yields bounded packets from
  one logical stream.
* ``OggPacketWriter<W>`` — writes bounded pages for one logical stream with
  packet granules and BOS/EOS flags; call ``finish()`` to write the EOS page.
* ``OggPacket`` — packet bytes plus page sequence, granule position, named BOS,
  EOS, and continuation flags, and packet/page boundary information.

Crate ``zemer-core``
--------------------

Streaming decoder and encoder interfaces. It re-exports metadata and PCM types
for convenience and compatibility:

* ``AudioDecoder`` and ``AudioEncoder`` — block-based read and write traits;
  ``AudioEncoder::finish`` finalizes streaming output. ``DsdDecoder`` and
  ``DsdEncoder`` expose native one-bit audio without passing it through the PCM
  API.
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

Crate ``zemer-dsf``
-------------------

* ``DsfDecoder<R>`` and ``DsfEncoder<W>`` — streaming DSF 1.01 raw DSD for the
  documented channel layouts at 2.8224/5.6448 MHz. Encoder input uses
  channel-interleaved, MSB-first octets; the writer serializes DSF's required
  per-channel blocks and LSB-first bytes.
* The decoder reads common ID3v2.3/v2.4 text fields. The encoder writes ID3v2.4
  UTF-8 text frames and stores unknown metadata keys in ``TXXX`` frames.

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
  emits lossless verbatim or fixed-predictor/Rice subframes and patches
  STREAMINFO totals and MD5 at ``finish()``. Floating-point PCM is not
  supported by this encoder.
* ``FlacDecoder<R>`` — streaming decoder for native FLAC streams; reads
  Vorbis comments into shared ``Metadata`` and exposes retained raw blocks via
  ``metadata_blocks()``.
* ``OggFlacDecoder<R>`` — Ogg-FLAC mapping decoder using the same PCM and
  metadata interface.
* ``OggFlacEncoder<W>`` — streaming Ogg-FLAC writer for integer PCM. It emits
  one frame packet at a time and writes the final sample count to the EOS page.
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
* ``WavPackDecoder`` — implements ``AudioDecoder`` for lossless integer PCM at
  8, 16, 24, and 32 bits, stereo lossless IEEE float32, custom
  sample rates, false stereo, and multichannel lossless PCM. It also decodes
  mono/stereo hybrid-lossy PCM16 and float32, hybrid-lossless PCM16/24/32 and
  float32 with paired ``.wvc`` files, and shaped/cross-channel hybrid PCM16. Tests
  cover 3–8 channel PCM16 groups, 3-channel hybrid PCM16, and 5.1 PCM24/32. Pair
  correction streams with ``WavPackDecoder::new_with_correction``. WavPack v5
  block, decoded-sample, and integer/float WVX checksums are verified.
* ``WavPackDecoder::new_seekable`` loads trailing APEv2 text metadata before
  decoding; the streaming constructor does not seek to the file end.
* ``read_apev2_tag`` and ``rewrite_metadata`` read or rewrite trailing APEv2
  tags while preserving non-text items, compressed audio bytes, and ID3v1 data.
* ``WavPackDsdDecoder`` — implements ``DsdDecoder`` for native DSD direct, fast,
  and adaptive profiles, verifying each decoded block checksum. Oracle tests
  cover all three profiles in stereo and adaptive DSD in 5.1.
* ``WavPackEncoder`` — implements ``AudioEncoder`` for lossless integer PCM at
  8/16/24/32 bits and up to 32 channels, plus bit-exact float32 encoding. The
  float path carries sample bits uncompressed in WVX.
* ``WavPackHybridEncoder`` — implements ``AudioEncoder`` for integer PCM hybrid
  output at 8/16/24/32 bits. It writes a bitrate-targeted ``.wv`` stream and
  its lossless ``.wvc`` correction stream; valid target rates are 24–9600 kbps.
* ``WavPackDsdEncoder`` — implements ``DsdEncoder`` for raw native DSD octets
  for up to 32 channels.

The multichannel block-group fixtures cover 3–8 channel PCM16. WavPack DSD is
always lossless and has no hybrid ``.wvc`` correction mode.
Hybrid-lossless restoration has conformance coverage for stereo PCM16/24/32
and float32, including shaped and cross-channel PCM16 profiles.
The float32 conformance fixture checks exact IEEE-754 bit patterns including
signed zero, subnormals, infinities, and NaNs. The crate uses only Rust's
standard library, ``zemer-core``, and ``zemer-metadata``; no external codec
implementation is linked.

Generate complete local Rust API documentation from the repository root:

.. code-block:: console

   cargo doc --workspace --no-deps --open
