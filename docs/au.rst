Sun/NeXT AU
===========

The ``zemer-au`` crate reads and writes the documented Sun/NeXT AU encodings
listed here. AU files start with a 24-byte ``.snd`` header and at least four
annotation bytes, so the audio data offset must be at least 28. Header integers,
linear PCM samples, and IEEE floating-point samples use big-endian byte order.
The format's data offset locates the annotation and audio; a data size of
``0xffffffff`` means that audio continues to end of file.

Supported encodings are signed linear PCM 8/16/24/32-bit, IEEE float32/64,
G.711 μ-law, and G.711 A-law. Other AU encoding codes return an unsupported
error. Audio is decoded and encoded in bounded blocks. The decoder rejects
invalid offsets, truncated declared audio, and payloads that end in a partial
sample frame.

The raw annotation is available through ``AuDecoder::annotation()`` and can be
set before writing samples with ``AuEncoder::set_annotation()``. Valid UTF-8
text before the first NUL byte maps to one ``COMMENT`` tag. Metadata editing
copies the encoded audio without decoding it; if the COMMENT value is
unchanged, the original annotation bytes are retained exactly. An edited
COMMENT is written as UTF-8 followed by a NUL terminator and alignment padding.

The CLI detects AU by its ``.snd`` signature, regardless of the filename
extension. Use ``--format au`` for sample-format-selected AU output, or
``--format au_alaw`` and ``--format au_mulaw`` for explicit G.711 output.
Only COMMENT metadata has an AU annotation mapping.

The format profile follows the [Oracle AU documentation] and is checked
against the [McGill AU overview] and [McGill AU sample list]. The McGill page's
endianness sentence conflicts with Oracle's AU header and data description;
this implementation follows Oracle's documented big-endian layout.

.. [Oracle AU documentation] https://docs.oracle.com/cd/E19253-01/816-5174/6mbb98ucf/index.html
.. [McGill AU overview] https://www.mmsp.ece.mcgill.ca/Documents/AudioFormats/AU/AU.html
.. [McGill AU sample list] https://www.mmsp.ece.mcgill.ca/Documents/AudioFormats/AU/Samples.html
