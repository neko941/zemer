Native FLAC
===========

The ``zemer-flac`` crate provides streaming encoding and decoding for native
FLAC files. It implements ``zemer_core::AudioDecoder`` and
``zemer_core::AudioEncoder`` using shared interleaved PCM blocks, so
applications can use it without the Zemer CLI.

The current format reference is `RFC 9639 <https://datatracker.ietf.org/doc/html/rfc9639>`_.
The supplied `draft-ietf-cellar-flac-08 <https://www.ietf.org/archive/id/draft-ietf-cellar-flac-08.pdf>`_
is an expired 2023 draft and is useful as a detailed companion, not as the
current normative text. The encoder currently writes integer PCM using
verbatim subframes; it does not search for predictors or claim the FLAC
streamable subset.

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

The ``FlacEncoder<W>`` accepts integer PCM at 8, 16, 24, and 32 bits, streams
4096-sample verbatim frames, writes Vorbis comments, and patches STREAMINFO
sample totals and MD5 when ``finish()`` succeeds. It requires a seekable output
and has no floating-point path. Optional channel masks are written as the
RFC-defined hexadecimal Vorbis comment. The decoder reads STREAMINFO and
Vorbis comments, retains each metadata block's
raw type and payload through ``metadata_blocks()``, handles constant, verbatim,
fixed-predictor, and linear-predictor subframes, Rice residuals, wasted bits,
and stereo decorrelation. It verifies frame header and frame CRCs. It supports
native FLAC and single-logical-stream Ogg-FLAC decoding; chained logical streams
and seeking are not implemented. Ogg-FLAC validates page framing and checks the
terminal granule against the decoded sample count, including when STREAMINFO's
total is unknown, and trims trailing samples from the final decoded block as
required. The CLI can inspect either
FLAC mapping and convert them to WAVE; encoding is currently exposed by the Rust
library only.
Support is partial: valid uncommon streams outside the decoder's current
capabilities may be rejected. Vorbis comments are exposed as shared text
metadata. Every block is preserved as its original type and payload. Picture
blocks can additionally be parsed with ``FlacMetadataBlock::picture()`` into
borrowed fields and image/URI bytes; URI values are reported but never fetched.
Application blocks can be parsed with ``FlacMetadataBlock::application()`` into
their four-byte ID and borrowed payload. Seek tables can be read through
``FlacMetadataBlock::seektable()`` as borrowed points, including placeholder
points, without copying the table. Cuesheet blocks can be parsed with
``FlacMetadataBlock::cuesheet()`` into track and index records; the parser
checks reserved fields, track layout, lead-out, ISRC, and CD-DA sector-alignment
rules. ``rewrite_metadata()`` replaces or adds the Vorbis comment block while
preserving all other metadata block payloads and copying the encoded audio
bytes unchanged. It writes a new stream; callers should use a temporary output
file before replacing a source file. Retained metadata is limited to 64 MiB
per stream.
It rejects streams that change sample rate, channel count,
or bit depth between frames because the shared decoder interface has one fixed
``AudioSpec`` per stream. After the caller reads through end-of-stream, the
decoder verifies the interleaved signed PCM bytes against the STREAMINFO MD5
when that checksum is present. A mismatch returns an error on the final read;
an all-zero (unknown) checksum is skipped. Consumers that stop early do not
receive whole-stream MD5 verification.

When STREAMINFO does not specify a maximum frame size, a frame is limited to
16,777,215 bytes to bound decoder memory. A valid file exceeding that limit
will be rejected.
