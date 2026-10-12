Design
======

Zemer separates format-independent audio data from format-specific parsers and
writers:

* ``zemer-pcm`` defines sample formats, audio specifications, PCM buffers, and
  sample-format conversion.
* ``zemer-core`` defines metadata and streaming decoder/encoder traits, and
  re-exports the PCM types for convenience.
* ``wav`` implements RIFF/WAVE on top of those shared interfaces.
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
