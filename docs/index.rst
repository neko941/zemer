Zemer
=====

Zemer is a clean-room Rust library for reading, writing, and converting audio
files. Its design keeps shared audio types independent from format-specific
crates, so applications can use only the pieces they need.

The project implements RIFF/WAVE PCM, IEEE float, and G.711 reading and
writing, RIFF INFO metadata, Broadcast Wave ``bext`` chunks, and CUE sheets
for WAVE tracks. Its FLAC crate streams integer PCM
to and from native FLAC files and decodes, encodes, and edits metadata in
Ogg-FLAC.
The ``zemer-wavpack`` crate decodes lossless PCM, float32, hybrid PCM/float with
paired ``.wvc`` correction, and native DSD profiles. It writes lossless integer
PCM at 8/16/24/32 bits for up to 32 channels, preserves float32 bits through
uncompressed WVX, writes raw native DSD for up to 32 channels, and emits
standard-bitrate integer PCM hybrid pairs. APEv2 UTF-8 text tags can be edited
while retaining non-text items. The official suite decoded all 35 supported
files, skipped 9 legacy files, and rejected all 4 malformed files.
The ``zemer-aiff`` crate reads AIFF/AIFC integer PCM and AIFC IEEE float32/64,
and G.711 A-law/μ-law; it writes AIFF integer PCM and AIFC integer/float/G.711
profiles. AIFF text chunks map to shared title, artist, and comment metadata;
typed comments and sample-frame markers are also exposed and writable.
The shared ``zemer-g711`` crate provides the companding functions for WAVE and
AIFF-C.
The ``zemer-dsf`` crate reads and writes raw DSF 1.01 DSD streams for its
documented channel/rate profile and maps common ID3 text fields. The
``zemer-dsdiff`` crate reads raw and DST-compressed DSDIFF/DFF audio, writes
uncompressed DSD and compressed DST with a valid bypass fallback, and supports
title, artist, comments, channel IDs, and typed markers. Available ISO DST
vectors pass bit-exactly at DSD64/128/256.

.. toctree::
   :maxdepth: 2
   :caption: User guide

   getting-started
   wav
   aiff
   flac
   wavpack
   dsf
   dsdiff
   ogg
   cue
   cli
   architecture
   api

The implementation is written from published format documentation and original
tests. See the repository README for project status and contribution details.

The repository root's `ROADMAP.md` lists planned codecs, conversion, metadata,
and disc-image workflows.
