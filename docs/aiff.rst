AIFF and AIFC
=============

The ``zemer-aiff`` crate reads seekable AIFF/AIFC streams and writes seekable
AIFF integer PCM or AIFC integer/IEEE-float streams. It implements
``AudioDecoder`` and ``AudioEncoder`` and reuses ``zemer-core``, ``g711``, and
``g726`` for shared models and codecs.

The decoder accepts integer sample widths from 1 through 32 bits, preserving
left-aligned valid bits inside the shared 8/16/24/32-bit sample containers. The
library writer can select a valid bit depth from 1 through 32 with
``AiffEncoder::new_with_bits_per_sample`` or
``AiffEncoder::new_aifc_with_bits_per_sample``. The selected depth must fit the
input sample container (8, 16, 24, or 32 bits); unused low bits are cleared,
which truncates precision. Existing constructors write the full container
width. The decoder also reads big-endian AIFF PCM, AIFC ``NONE``/``twos``
big-endian PCM, AIFC ``sowt`` little-endian PCM, and AIFC ``fl32``/``fl64``
IEEE floating-point audio (including uppercase ``FL32``/``FL64`` aliases).
The writer emits AIFF integer PCM and AIFC integer
PCM at the selected valid depth, or float32/64 at full precision.
Apple IMA4 AIFC packets decode to signed 16-bit PCM; ``new_aifc_ima4`` writes
IMA4 only when explicitly selected. Each packet contains 64 frames per channel.
AIFF-C stores the packet count in COMM for this profile, so the final packet
may add up to 63 decoded padding frames.
It also decodes AIFC A-law/μ-law using both Apple lowercase and SGI uppercase
compression IDs, and writes the lowercase Apple IDs when selected through
``new_aifc_g711``. G.711 expands to signed 16-bit PCM and lossy encoding accepts
only signed 16-bit input.
The library decodes and encodes AIFC ``G726`` at 16, 24, 32, and 40 kbit/s.
Both directions require 8 kHz signed 16-bit PCM. The McGill ``stocG726`` APPL
hint supplies the rate for decoding; other files must use
``AiffDecoder::new_with_g726_rate`` because AIFF-C does not define a general
G.726 bitrate field. ``AiffEncoder::new_aifc_g726`` takes an explicit rate and
writes the same hint. The ``G726Rate`` enum is re-exported from the ``aiff``
crate. The clean-room encoder matches an independent SpanDSP codeword oracle at
all four rates.
The AIFC ``DWVW`` compression code is decoded and encoded at 2–32 bits per
sample. ``AiffDecoder`` scans the channel-sequential sound data once to find
each channel boundary, then returns interleaved PCM blocks. The library writer
provides ``new_aifc_dwvw`` and ``new_aifc_dwvw_with_bits_per_sample``; because
DWVW stores channel planes, it spools samples to temporary files until finish.
The clean-room decoder matches a published 16-bit example against an
independently generated payload. A generated mono 24-bit encoder profile also
matches libsndfile 1.2.2 sample-for-sample; broader DWVW corpus and multichannel
writer interoperability checks remain open.

AIFF-C output includes the required ``FVER`` chunk. Both readers and writers
map ``NAME``, ``AUTH``, ``(c) ``, and repeated ``ANNO`` chunks to TITLE,
ARTIST, COPYRIGHT, and COMMENT. The copyright chunk is ASCII text and may
appear at most once.

The reader also accepts one ``ID3 `` chunk as a compatibility extension. The
chunk payload is a bounded ID3v2 tag parsed by the reusable ``zemer-id3``
crate. Common text fields map to ``Metadata``; other frames remain opaque when
an AIFF/AIFC file is copied or its text tags are edited. The encoder writes an
ID3 chunk for metadata fields that have no native AIFF text chunk. ID3-in-AIFF
is outside the AIFF chunk set described by the McGill specification, so files
that require strict AIFF interoperability should use the native text chunks.

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
padding, IDs, positions, and duplicate chunks. CLI conversion to AIFF or AIFC
carries typed comments and markers to the output; metadata edits preserve the
original COMT and MARK chunks byte-for-byte.

The typed ``AiffInstrument`` API reads and writes the standard 20-byte
``INST`` chunk. It exposes MIDI note and velocity ranges, signed cents detune,
gain in decibels, and optional forward or forward/backward sustain and release
loops. Active loops must refer to marker IDs in ``MARK``. Although the AIFF
specification gives a low velocity range of 1 through 127, the reader and writer
also accept zero because Typhoon-generated sampler files use 0–127 for the full
range. Other ``INST`` chunk sizes are skipped because the Apple IIGS format uses
the same chunk ID for a different structure. AIFF/AIFC CLI conversions carry
the standard instrument chunk alongside markers and comments; metadata edits
preserve it byte-for-byte.

The optional AIFC ``AESD`` audio-recording chunk is exposed as
``AiffAudioRecording``. Its AES channel-status data is retained as exactly 24
raw bytes; the decoder rejects wrong-sized or duplicate chunks, and the encoder
can write it with ``set_audio_recording``. CLI conversion between AIFC files
preserves this chunk. Converting to AIFF omits it because ``AESD`` is an AIFC
extension.

For compatibility with McGill's ``digit0v0.aiff`` sample, the reader accepts
an AIFC ``COMM`` size of 18 when the immediately following bytes are exactly the
``NONE`` compression code and its canonical ``not compressed`` name. It then
continues parsing after those out-of-bound fields. Other incomplete AIFC
``COMM`` sizes remain invalid.

Chunk bounds, odd-size padding, the AIFC version, the extended sample rate,
SSND offsets, COMM frame counts, and metadata size are checked. ``SSND`` is
required for non-empty audio and may be omitted when ``COMM`` declares zero
frames. Unknown chunks are skipped. AIFC MACE and other compression codes are
not implemented; the writer does not preserve unknown chunks from a source
file.
``rewrite_metadata`` replaces the supported text chunks while copying the
audio bytes and unrecognized chunks without decoding them. It updates a
present ID3 chunk while retaining opaque frames, or adds one for non-native
metadata. It also retains bytes after the FORM boundary.

The optional ignored ``decodes_external_12_bit_aiff_and_aifc_against_pcm_oracles``
test compares McGill/AFsp 12-bit AIFF and AIFC sample files with separate FFmpeg
signed-16 PCM oracles. Both samples contain 23,493 stereo frames at 8 kHz. The
sample and oracle paths are supplied through ``ZEMER_AIFF_12BIT_SAMPLE``,
``ZEMER_AIFF_12BIT_PCM``, ``ZEMER_AIFC_12BIT_SAMPLE``, and
``ZEMER_AIFC_12BIT_PCM``; the files are temporary test inputs and are not
bundled.

Additional ignored external checks compare 15 McGill AFsp profiles, six AIFC
edge cases, seven older CCRMA files, and the McGill 40-kbit/s G.726 sample with
FFmpeg or SpanDSP PCM oracles. These corpus checks have passed on server5; the
sample files remain external because their redistribution terms are not stated.
Download commands and test names are recorded in ``tests/fixtures/README.md``.

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
16-bit input and reports packet-padded output frames. Select
``aifc_g726 --bitrate 16|24|32|40`` for G.726 output; it requires 8 kHz input
and encodes signed 16-bit PCM.
Use ``aifc_dwvw`` for AIFC DWVW output; it accepts integer PCM and optional
``--valid-bits`` from 2 through 32, subject to the input container width.
