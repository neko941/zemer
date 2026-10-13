Design
======

Zemer separates format-independent audio data from format-specific parsers and
writers:

* ``zemer-pcm`` defines sample formats, audio specifications, PCM buffers, and
  sample-format conversion.
* ``zemer-metadata`` defines format-independent tags shared by format crates.
* ``zemer-core`` defines streaming decoder/encoder traits and re-exports the
  metadata and PCM types for convenience.
* ``g711`` contains format-independent A-law/μ-law sample conversion reused by
  WAVE and AIFF-C.
* ``ogg`` validates Ogg page framing/CRC and exposes bounded packets; codec
  mapping rules remain in their own crates.
* ``wav`` implements RIFF/WAVE and ``aiff`` implements AIFF/AIFC on top of
  those shared interfaces.
* ``dsf`` and ``dsdiff`` implement separate raw DSD container layouts on top
  of the shared packed-octet DSD interface.
* ``dst`` is the reusable compressed DSD frame codec used by ``dsdiff``; it
  does not depend on the DSDIFF container.
* ``flac`` decodes native FLAC frames and uses ``ogg`` for the Ogg-FLAC
  mapping, producing the same shared PCM and metadata types.
* ``cue`` parses and writes CUE sheet models without depending on a codec or
  audio container implementation.

Format crates and codecs should remain independent when multiple containers
can carry the same audio encoding. For example, PCM can be used by WAV, AIFF,
and AU, while CUE describes track locations and metadata without decoding audio.

Sample-format conversion, resampling, channel mixing, and lossy encoding are
separate operations. Whole-file conversion composes a source decoder, shared
PCM blocks, and a target encoder. The current ``transcode`` helper copies
blocks only when input and output sample specifications match; it does not
change audio values.

Each format crate parses metadata from its own container and maps it to the
shared ``Metadata`` type. Format-specific structures such as FLAC STREAMINFO
remain in their format crate.
