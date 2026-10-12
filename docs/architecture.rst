Design
======

Zemer separates format-independent audio data from format-specific parsers and
writers:

* ``zemer-core`` defines sample formats, audio specifications, metadata, and
  streaming decoder/encoder traits.
* ``zemer-wav`` implements RIFF/WAVE on top of those shared interfaces.

The core crate does not identify files by extension. A future format crate can
depend on ``zemer-core`` without pulling in other format implementations.

PCM conversion, resampling, channel mixing, and lossy encoding are separate
operations. The current ``transcode`` helper copies blocks only when input and
output sample specifications match; it does not change audio values.
