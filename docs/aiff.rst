AIFF and AIFC
=============

The ``zemer-aiff`` crate reads seekable AIFF/AIFC streams and writes seekable
AIFF integer PCM or AIFC integer/IEEE-float streams. It implements
``AudioDecoder`` and ``AudioEncoder`` and depends only on ``zemer-core``.

The decoder accepts integer sample widths from 1 through 32 bits, preserving
left-aligned valid bits inside the shared 8/16/24/32-bit sample containers. It
reads big-endian AIFF PCM, AIFC ``NONE``/``twos`` big-endian PCM, AIFC ``sowt``
little-endian PCM, and AIFC ``fl32``/``fl64`` IEEE floating-point audio. The
writer emits full-width AIFF integer PCM and AIFC integer PCM or float32/64.
Apple IMA4 AIFC packets decode to signed 16-bit PCM; ``new_aifc_ima4`` writes
IMA4 only when explicitly selected. Each packet contains 64 frames per channel.
AIFF-C stores the packet count in COMM for this profile, so the final packet
may add up to 63 decoded padding frames.
It also decodes AIFC A-law/μ-law using both Apple lowercase and SGI uppercase
compression IDs, and writes the lowercase Apple IDs when selected through
``new_aifc_g711``. G.711 expands to signed 16-bit PCM and lossy encoding accepts
only signed 16-bit input.
AIFF-C output includes the required ``FVER`` chunk. Both readers and writers
map ``NAME``, ``AUTH``, and repeated ``ANNO`` chunks to TITLE, ARTIST, and
COMMENT.

The typed ``AiffComment`` API reads and writes the AIFF ``COMT`` chunk while
retaining each record's 1904-epoch timestamp, signed marker ID, and raw text
bytes. The generic metadata map does not flatten these records, so timestamp
and marker associations remain intact. At most one ``COMT`` chunk is accepted;
record counts, text lengths, padding, and the aggregate metadata limit are
validated.

The typed ``AiffMarker`` API reads and writes the optional ``MARK`` chunk.
Marker IDs are unique positive signed 16-bit values, positions count sample
frames (including the valid end position), and names retain their Pascal-string
bytes without assuming a text encoding. The decoder validates record bounds,
padding, IDs, positions, and duplicate chunks. Same-format CLI conversion to
AIFF or AIFC carries these markers to the output; the metadata rewrite also
preserves the original MARK chunk byte-for-byte.

Chunk bounds, odd-size padding, the AIFC version, the extended sample rate,
SSND offsets, COMM frame counts, and metadata size are checked. ``SSND`` is
required for non-empty audio and may be omitted when ``COMM`` declares zero
frames. Unknown chunks are skipped. AIFC MACE and other compression codes are
not implemented; the writer does not preserve unknown chunks from a source
file.
``rewrite_metadata`` replaces the supported text chunks while copying the
audio bytes and unrecognized chunks without decoding them. It also retains
bytes after the FORM boundary.

The optional ignored ``decodes_external_12_bit_aiff_and_aifc_against_pcm_oracles``
test compares McGill/AFsp 12-bit AIFF and AIFC sample files with separate FFmpeg
signed-16 PCM oracles. Both samples contain 23,493 stereo frames at 8 kHz. The
sample and oracle paths are supplied through ``ZEMER_AIFF_12BIT_SAMPLE``,
``ZEMER_AIFF_12BIT_PCM``, ``ZEMER_AIFC_12BIT_SAMPLE``, and
``ZEMER_AIFC_12BIT_PCM``; the files are temporary test inputs and are not
bundled.

.. code-block:: rust

   use std::fs::File;
   use aiff::AiffDecoder;
   use zemer_core::AudioDecoder;

   let mut decoder = AiffDecoder::new(File::open("input.aiff")?)?;
   while let Some(block) = decoder.read_frames(4096)? {
       println!("read {} frames", block.frames());
   }

The CLI identifies AIFF and AIFC by their FORM signature, so filenames do not
need conventional extensions. It can inspect either and convert to ``aiff``
or ``aifc`` output. AIFF output accepts integer PCM. AIFC output additionally
accepts float32/64 and preserves those sample bits. ``aifc_ima4`` encodes lossy
16-bit input and reports packet-padded output frames.
