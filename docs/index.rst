Zemer
=====

Zemer is a clean-room Rust library for reading, writing, and converting audio
files. Its design keeps shared audio types independent from format-specific
crates, so applications can use only the pieces they need.

The project implements RIFF/WAVE PCM, IEEE float, and G.711 reading and
writing, plus CUE sheets for WAVE tracks. A native FLAC streaming decoder is
available through its Rust crate.
The ``zemer-wavpack`` crate decodes mono and stereo lossless integer PCM at 8,
16, 24, and 32 bits, stereo lossless IEEE float32, 5.1 and 7.1 lossless PCM16,
and mono and stereo unshaped hybrid-lossy PCM16. Stereo noise-shaped hybrid-lossy
PCM16 and stereo PCM16 hybrid-lossless restoration with a paired ``.wvc`` file
are tested for flat and shaped profiles. Cross-channel correction, other
multichannel layouts, and DSD remain incomplete.

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
