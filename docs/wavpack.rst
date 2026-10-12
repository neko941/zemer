WavPack
=======

The ``zemer-wavpack`` crate decodes WavPack PCM and native DSD without linking
an external codec implementation. It also reads and rewrites trailing APEv2
tags. The CLI can inspect WavPack streams, convert PCM output to WAVE, and edit
APEv2 text metadata.

APEv2 text fields are UTF-8 and case-insensitive when looked up. Rewriting
metadata preserves binary, external, and reserved items, copies compressed
audio blocks byte-for-byte, and retains a trailing ID3v1 record. Tags are
bounded to 64 MiB. Streaming ``WavPackDecoder::new`` does not seek to find
trailing tags; use ``WavPackDecoder::new_seekable`` when the input supports
seeking and metadata must be available before decoding.

.. code-block:: console

   zemer inspect --json album.wv
   zemer convert --format pcm_i16 album.wv album.wav
   zemer metadata edit --set "TITLE=New title" album.wv tagged.wv

The crate does not yet encode WavPack or expose binary cover-art editing through
the shared ``Metadata`` model. Native DSD remains available through
``WavPackDsdDecoder`` and is not converted implicitly to PCM.
