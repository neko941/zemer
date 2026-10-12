Zemer
=====

Zemer is a clean-room Rust library for reading, writing, and converting audio
files. Its design keeps shared audio types independent from format-specific
crates, so applications can use only the pieces they need.

The project is in early development. The first implemented format is RIFF/WAVE
PCM, IEEE float, and G.711 decoding and encoding.

.. toctree::
   :maxdepth: 2
   :caption: User guide

   getting-started
   wav
   cli
   architecture
   api

The implementation is written from published format documentation and original
tests. See the repository README for project status and contribution details.

The repository root's `ROADMAP.md` lists planned codecs, conversion, metadata,
and disc-image workflows.
