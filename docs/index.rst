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
The ``zemer-aiff`` crate reads AIFF/AIFC integer PCM, including AIFC
little-endian ``sowt`` output, AIFC IEEE float32/64,
G.711 A-law/μ-law, Apple IMA4, DWVW, and G.726; it writes AIFF integer PCM and
AIFC big-endian/little-endian PCM, float/G.711/IMA4/DWVW/G.726 profiles. The reusable
``zemer-dwvw`` crate provides the channel codec. AIFF text chunks map to shared
title, artist, and comment metadata;
typed comments and sample-frame markers are also exposed and writable.
The shared ``zemer-g711`` crate provides the companding functions for WAVE,
AIFF-C, and CAF. The shared ``zemer-id3`` crate provides bounded ID3 text parsing and
rewriting for DSF and AIFF/AIFC's non-standard ``ID3 `` chunk.
The ``zemer-au`` crate reads and writes Sun/NeXT AU signed PCM, IEEE float32/64,
and G.711 A-law/μ-law with big-endian samples. It preserves bounded raw
annotations and maps valid UTF-8 annotation text to COMMENT metadata.
The ``zemer-caf`` crate reads and writes CAF v1 PCM/float, G.711, and Apple
IMA4 profiles, with UTF-8 metadata and audio-preserving metadata edits.
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
   au
   caf
   raw-pcm
   flac
   wavpack
   dsf
   dsdiff
   ogg
   cue
   iso9660
   cli
   architecture
   api

The implementation is written from published format documentation and original
tests. See the repository README for project status and contribution details.

The repository root's `ROADMAP.md` lists planned codecs, conversion, metadata,
and disc-image workflows.
