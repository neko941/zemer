Native FLAC
===========

The ``zemer-flac`` crate provides streaming encoding and decoding for native
FLAC files. It implements ``zemer_core::AudioDecoder`` and
``zemer_core::AudioEncoder`` using shared interleaved PCM blocks, so
applications can use it without the Zemer CLI.

The current format reference is `RFC 9639 <https://datatracker.ietf.org/doc/html/rfc9639>`_.
The supplied `draft-ietf-cellar-flac-08 <https://www.ietf.org/archive/id/draft-ietf-cellar-flac-08.pdf>`_
is an expired 2023 draft and is useful as a detailed companion, not as the
current normative text. The encoder writes integer PCM using verbatim or
fixed-predictor subframes with Rice-coded residuals. It picks a predictor only
when the encoded frame is smaller than the verbatim form.

.. code-block:: rust

   use std::{fs::File, io::BufReader};
   use flac::FlacDecoder;
   use zemer_core::AudioDecoder;

   let mut decoder = FlacDecoder::new(BufReader::new(File::open("input.flac")?))?;
   println!("{} Hz, {} channels", decoder.spec().sample_rate(), decoder.spec().channels());
   while let Some(block) = decoder.read_frames(4096)? {
       println!("{} frames", block.frames());
   }
   # Ok::<(), zemer_core::AudioError>(())

For random access, construct the decoder with ``new_seekable()`` and seek to a
zero-based sample frame. A valid SEEKTABLE is used to jump near the target;
without one, the decoder seeks to the first audio frame and decodes forward.

.. code-block:: rust

   use std::{fs::File, io::BufReader};
   use flac::FlacDecoder;
   use zemer_core::AudioDecoder;

   let mut decoder = FlacDecoder::new_seekable(BufReader::new(File::open("input.flac")?))?;
   decoder.seek_to_sample(44_100)?;
   if let Some(block) = decoder.read_frames(4096)? {
       println!("{} frames starting at 1 second", block.frames());
   }
   # Ok::<(), zemer_core::AudioError>(())

Seeking from sample zero allows normal full-stream MD5 verification. Resuming
from a later SEEKTABLE point skips the prefix, so the full-stream MD5 cannot be
verified. The regular
``new()`` constructor remains available for readers that only implement
``Read``.

The ``FlacEncoder<W>`` accepts integer PCM at 8, 16, 24, and 32 bits, streams
4096-sample verbatim or fixed-predictor/Rice frames, writes Vorbis comments,
and patches STREAMINFO sample totals and MD5 when ``finish()`` succeeds. It
requires a seekable output and has no floating-point path. Optional channel
masks are written as the RFC-defined hexadecimal Vorbis comment. The decoder reads STREAMINFO and
Vorbis comments, retains each metadata block's
raw type and payload through ``metadata_blocks()``, handles constant, verbatim,
fixed-predictor, and linear-predictor subframes, Rice residuals, wasted bits,
and stereo decorrelation. It verifies frame header and frame CRCs. It supports
native FLAC and chained Ogg-FLAC decoding. Ogg-FLAC links may have different
audio specifications; ``spec()`` and each returned PCM
block report the active link's specification, and the decoder keeps metadata
from the first link.
For seekable inputs, ``OggFlacDecoder::new_seekable`` and
``seek_to_sample`` restart from the initial Ogg offset and decode forward.
CLI inspection and conversion require every chained link to share one audio
specification because a single WAVE or FLAC output has one fixed PCM format.
Ogg-FLAC validates page framing and checks the terminal granule against the
decoded sample count plus the initial granule offset, including when
STREAMINFO's total is unknown. It trims trailing samples from the final
decoded block as required. For compatibility, it accepts an empty terminal EOS
packet when that packet carries the final granule, and rejects empty nonterminal
audio packets; Ogg-FLAC metadata rewriting preserves the terminal packet. The
CLI can inspect either FLAC mapping and convert
them to WAVE, native FLAC, or Ogg-FLAC. The ``OggFlacEncoder<W>`` writes one
packet per page, with bounded page sizes, terminal granules, and an EOS page.
The encoder supports integer PCM at 8, 16, 24, and 32 bits. Vorbis comments are exposed as shared text
metadata. Every block is preserved as its original type and payload. Picture
blocks can additionally be parsed with ``FlacMetadataBlock::picture()`` into
borrowed fields and image/URI bytes; URI values are reported but never fetched.
Application blocks can be parsed with ``FlacMetadataBlock::application()`` into
their four-byte ID and borrowed payload. Seek tables can be read through
``FlacMetadataBlock::seektable()`` as borrowed points, including placeholder
points, without copying the table. Cuesheet blocks can be parsed with
``FlacMetadataBlock::cuesheet()`` into track and index records; the parser
checks reserved fields, track layout, lead-out, ISRC, and CD-DA sector-alignment
rules. ``rewrite_metadata()`` replaces or adds the native FLAC Vorbis comment
block while preserving all other metadata block payloads and copying the
encoded audio bytes unchanged. ``rewrite_ogg_metadata()`` edits the first
Ogg-FLAC link's comments, preserves later chained links, and keeps each encoded
FLAC audio packet byte-for-byte while repaginating the Ogg stream. It writes a
new stream; callers should use a temporary output file before replacing a
source file. Retained metadata is limited to 128 MiB per stream.
The native FLAC decoder rejects streams that change sample rate, channel count,
or bit depth between frames because its ``AudioSpec`` is fixed per stream.
Ogg-FLAC permits those changes at link boundaries and updates ``spec()`` for
each link. After the caller reads through end-of-stream, the
decoder verifies the interleaved signed PCM bytes against the STREAMINFO MD5
when that checksum is present. A mismatch returns an error on the final read;
an all-zero (unknown) checksum is skipped. Consumers that stop early do not
receive whole-stream MD5 verification.

When STREAMINFO does not specify a maximum frame size, a frame is limited to
16,777,215 bytes to bound decoder memory. A valid file exceeding that limit
will be rejected.

Ogg-FLAC interoperability is tested against generated reference frames and
FFmpeg 8.1 encoded fixtures for mono 24-bit, stereo 16-bit, 5.1 24-bit, and
7.1 24-bit at 192 kHz, with exact PCM comparisons. FFmpeg also independently
decodes Zemer-generated output for those profiles with matching PCM hashes.
The multichannel fixtures include packets continued across Ogg pages. The
libFLAC 1.4.3 reference encoder also supplies synthetic 8-bit mono at 8 kHz
and 32-bit 7.1 at 192 kHz fixtures, checked sample-for-sample against their
deterministic source formulae. The
optional CC0 decoder-testbench check re-encodes all accepted corpus cases as
Ogg-FLAC and compares their decoded PCM exactly.

The decoder also accepts the tested pre-1.1.1 legacy packet layout. Xiph notes
that these historical mappings were never formalized and recommends decoding
and re-encoding them with a current FLAC implementation. Zemer writes only the
current mapping. Metadata rewriting accepts the tested legacy layout and emits
the current mapping while preserving every encoded audio packet. Two legacy
samples from the FFmpeg sample archive were decoded on Linux and compared
sample-for-sample with libFLAC PCM oracles; the same test rewrites their comments
and checks that output streams decode through EOF with the same frame counts. The ignored
``decodes_legacy_ogg_flac_against_libflac_pcm_oracle`` test uses
``ZEMER_OGG_FLAC_LEGACY_SAMPLE`` and ``ZEMER_OGG_FLAC_LEGACY_PCM``. Neither is
bundled. Six current-mapping samples have optional ignored tests:
``decodes_external_ogg_flac_against_ffmpeg_pcm_oracle`` checks an FFmpeg sample,
``decodes_external_commons_ogg_flac_against_libflac_pcm_oracle`` checks Jono
Bacon's 2011 upload under CC BY-SA 2.0, and
``decodes_external_chopin_ogg_flac_against_libflac_pcm_oracle`` checks Muriel
Nguyen Xuan's 2007 recording under CC BY-SA 4.0. The latter's reference libFLAC
1.2.1 encoded file was decoded with libFLAC 1.4.3 to a signed 16-bit PCM oracle.
``decodes_external_public_domain_4m33s_ogg_flac_against_libflac_pcm_oracle``
checks John Cage's public-domain generated performance, compared against a
libFLAC signed 24-bit PCM oracle, and
``decodes_external_nasa_martian_dust_devil_against_libflac_pcm_oracle`` checks
NASA's public-domain SuperCam recording, and
``decodes_external_public_domain_wapichan_speech_against_libflac_pcm_oracle``
checks a 24-bit stereo Wapichan speech recording labelled public domain in the
United States. None of these recordings or oracles is bundled. All six tests
compare every decoded sample with an independent PCM oracle. These external
samples do not replace a broader independent Ogg-FLAC corpus.
