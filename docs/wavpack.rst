WavPack
-------

The ``zemer-wavpack`` crate decodes WavPack PCM, hybrid ``.wvc`` correction
streams, and native DSD without linking an external codec implementation. It
encodes lossless integer PCM for 1–32 channels at 8/16/24/32 bits, preserves
float32 sample bits through WVX, and writes raw native DSD for 1–32 channels.
The float path stores each IEEE value uncompressed in WVX. The crate also reads
and rewrites trailing APEv2 tags. The CLI can
inspect WavPack streams, convert PCM output to WAVE or WavPack, and edit APEv2
text metadata.

APEv2 text fields are UTF-8 and case-insensitive when looked up. Rewriting
metadata preserves binary, external, and reserved items, copies compressed
audio blocks byte-for-byte, and retains a trailing ID3v1 record. Tags are
bounded to 16 MiB. Streaming ``WavPackDecoder::new`` does not seek to find
trailing tags; use ``WavPackDecoder::new_seekable`` when the input supports
seeking and metadata must be available before decoding.

.. code-block:: console

   zemer inspect --json album.wv
   zemer convert --format pcm_i16 album.wv album.wav
   zemer convert --format wavpack source.wav archive.wv
   zemer metadata edit --set "TITLE=New title" album.wv tagged.wv

``WavPackEncoder`` and ``WavPackDsdEncoder`` expose the writers through the
shared PCM and DSD interfaces. Native DSD remains available through
``WavPackDsdDecoder`` and is not converted implicitly to PCM. Hybrid ``.wvc``
writing and binary cover-art editing through the shared ``Metadata`` model
remain unsupported.
