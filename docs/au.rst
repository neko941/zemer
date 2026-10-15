Sun/NeXT AU
===========

The ``zemer-au`` crate reads and writes the documented Sun/NeXT AU encodings
listed here. AU files start with a 24-byte ``.snd`` header and at least four
annotation bytes, so the audio data offset must be at least 28. Header integers,
linear PCM samples, and IEEE floating-point samples use big-endian byte order.
The format's data offset locates the annotation and audio; a data size of
``0xffffffff`` means that audio continues to end of file.

Supported encodings are signed linear PCM 8/16/24/32-bit, IEEE float32/64,
G.711 μ-law and A-law, and G.721/G.723 ADPCM. These ADPCM profiles are
restricted to mono 8 kHz audio and use the shared G.726 codec. G.721 packing is
low-bit-first. G.723 currently inherits low-bit-first packing from G.726, but
the Sun AU references checked so far identify codes 25/26 without defining
their payload bit order. G.721 writes complete two-sample groups; G.723 3-bit
and 5-bit writes require complete eight-sample groups because the AU header
does not store a decoded sample count. Other AU encoding codes return an
unsupported error. Audio is decoded and encoded in bounded blocks. The decoder
rejects invalid offsets, truncated declared audio, partial sample frames, and
ADPCM payloads that do not end on a complete packed group.

The raw annotation is available through ``AuDecoder::annotation()`` and can be
set before writing samples with ``AuEncoder::set_annotation()``. Valid UTF-8
text before the first NUL byte maps to one ``COMMENT`` tag. Metadata editing
copies the encoded audio without decoding it; if the COMMENT value is
unchanged, the original annotation bytes are retained exactly. An edited
COMMENT is written as UTF-8 followed by a NUL terminator and alignment padding.

AU encoding 23 uses the shared G.726 codec at 32 kbit/s; encodings 25 and 26
use its 24 kbit/s (3-bit) and 40 kbit/s (5-bit) rates
[Oracle G.723 AU encoding identifiers]_. Sun's G.721 utility
packs the first codeword into the low nibble [SunOS G.721 packing]_. RFC 3551
defines low-bit-first packing for G.726 RTP payloads, but that does not
establish the Sun AU G.723 byte layout [IETF RTP G.726 packing]_. In a
4,096-sample comparison using external SpanDSP codeword payloads, Zemer,
FFmpeg 4.2.2, and libsndfile 1.0.37 returned different PCM after initially
close output. Treat this as unresolved interoperability, not proof that one
decoder is correct. ITU-T G.726 replaces the 1988 G.721/G.723 recommendations
and defines the shared ADPCM rates [ITU-T G.726]_.

The CLI detects AU by its ``.snd`` signature, regardless of the filename
extension. Use ``--format au`` for sample-format-selected AU output,
``--format au_alaw`` and ``--format au_mulaw`` for explicit G.711 output, or
``--format au_g721``, ``au_g723_3``, and ``au_g723_5`` for mono 8 kHz
signed 16-bit ADPCM output.
Only COMMENT metadata has an AU annotation mapping.

The format profile follows the [Oracle AU documentation] and is checked
against the [McGill AU overview] and [McGill AU sample list]. The McGill page's
endianness sentence conflicts with Oracle's AU header and data description;
this implementation follows Oracle's documented big-endian layout.

.. [Oracle AU documentation] https://docs.oracle.com/cd/E19253-01/816-5174/6mbb98ucf/index.html
.. [Oracle G.723 AU encoding identifiers] https://docs.oracle.com/cd/A97385_01/appdev.920/a96120/annotator_parexample.htm
.. [McGill AU overview] https://www.mmsp.ece.mcgill.ca/Documents/AudioFormats/AU/AU.html
.. [McGill AU sample list] https://www.mmsp.ece.mcgill.ca/Documents/AudioFormats/AU/Samples.html
.. [SunOS G.721 packing] https://www.typewritten.org/Manual/Sun/SunOS/4.1.4/man1/adpcm_enc.html
.. [IETF RTP G.726 packing] https://www.rfc-editor.org/rfc/rfc3551.html#section-4.5.4
.. [ITU-T G.726] https://www.itu.int/rec/T-REC-G.726-199012-I%21%21PDF-E
