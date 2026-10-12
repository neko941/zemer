Design
======

Zemer separates format-independent audio data from format-specific parsers and
writers:

* ``zemer-core`` defines sample formats, audio specifications, PCM buffers,
  sample conversion, metadata, and streaming decoder/encoder traits.
* ``wav`` implements RIFF/WAVE on top of those shared interfaces.
* ``cue`` parses and writes CUE sheet models without depending on a codec or
  audio container implementation.

Format crates and codecs should remain independent when multiple containers
can carry the same audio encoding. For example, PCM can be used by WAV, AIFF,
and AU, while CUE describes track locations and metadata without decoding audio.

PCM conversion, resampling, channel mixing, and lossy encoding are separate
operations. The current ``transcode`` helper copies blocks only when input and
output sample specifications match; it does not change audio values.
