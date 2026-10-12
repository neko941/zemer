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
  values, usable without the decoder and encoder interfaces.

Crate ``zemer-core``
--------------------

Streaming decoder and encoder interfaces. It re-exports metadata and PCM types
for convenience and compatibility:

* ``AudioDecoder`` and ``AudioEncoder`` — block-based read and write traits.
* ``transcode`` — bounded block copying between matching PCM specifications,
  preserving metadata when the encoder supports it and returning an error if
  it cannot.

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
* ``WavWriter::new_with_metadata`` — create a streaming writer with title,
  artist, album, date, genre, and comment fields.
* ``G711Encoding`` and ``WavWriter::new_g711`` — explicitly encode signed
  16-bit PCM blocks as G.711 A-law or μ-law WAVE.
* ``WavContainer`` — explicitly choose classic RIFF or RF64 output.

Generate complete local Rust API documentation from the repository root:

.. code-block:: console

   cargo doc --workspace --no-deps --open
