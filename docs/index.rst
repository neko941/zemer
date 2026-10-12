Zemer
=====

Zemer is a clean-room Rust library for reading, writing, and converting audio
files. Its design keeps shared audio types independent from format-specific
crates, so applications can use only the pieces they need.

The project implements RIFF/WAVE PCM, IEEE float, and G.711 reading and
writing, plus CUE sheets for WAVE tracks. Its FLAC crate streams integer PCM
to and from native FLAC files.
The ``zemer-wavpack`` crate decodes mono and stereo lossless integer PCM at 8,
16, 24, and 32 bits, stereo lossless IEEE float32, custom sample rates, and
lossless PCM16 channel groups with 3–8 channels. Hybrid-lossy PCM16 and float32
are supported; stereo hybrid-lossless restoration with paired ``.wvc`` files is
tested for PCM16/24/32 and float32, including noise shaping and cross-channel
decorrelation. False stereo, a three-channel hybrid group, 5.1 PCM24/32, and
custom sample rates are also covered. Native DSD
direct/fast/adaptive stereo and adaptive 5.1 decode to packed, channel-interleaved
DSD octets.

.. toctree::
   :maxdepth: 2
   :caption: User guide

   getting-started
   wav
   flac
   cue
   cli
   architecture
   api

The implementation is written from published format documentation and original
tests. See the repository README for project status and contribution details.

The repository root's `ROADMAP.md` lists planned codecs, conversion, metadata,
and disc-image workflows.
