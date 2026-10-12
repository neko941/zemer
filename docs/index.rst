Zemer
=====

Zemer is a clean-room Rust library for reading, writing, and converting audio
files. Its design keeps shared audio types independent from format-specific
crates, so applications can use only the pieces they need.

The project implements RIFF/WAVE PCM, IEEE float, and G.711 reading and
writing, plus CUE sheets for WAVE tracks. Its FLAC crate streams integer PCM
to and from native FLAC files and decodes Ogg-FLAC.
The ``zemer-wavpack`` crate decodes lossless PCM, float32, hybrid PCM/float with
paired ``.wvc`` correction, and native DSD profiles. It writes lossless integer
PCM at 8/16/24/32 bits for up to 32 channels, preserves float32 bits through
uncompressed WVX, writes raw native DSD for up to 32 channels, and emits
standard-bitrate integer PCM hybrid pairs. APEv2 UTF-8 text tags can be edited
while retaining non-text items. The official suite decoded all 31 supported
files, skipped 13 legacy files, and rejected all 4 malformed files.

.. toctree::
   :maxdepth: 2
   :caption: User guide

   getting-started
   wav
   flac
   wavpack
   ogg
   cue
   cli
   architecture
   api

The implementation is written from published format documentation and original
tests. See the repository README for project status and contribution details.

The repository root's `ROADMAP.md` lists planned codecs, conversion, metadata,
and disc-image workflows.
