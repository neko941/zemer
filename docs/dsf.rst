DSF
===

The ``zemer-dsf`` crate reads and writes Sony DSF 1.01 raw DSD streams without
converting the samples to PCM. It supports both specified one-bit and eight-bit
storage orders, 2.8224 and
5.6448 MHz, mono through 5.1 layouts, and DSF's 4096-byte per-channel block
packing. Channel layout masks are checked before writing so the serialized
channel order remains explicit.

``DsfDecoder`` implements ``DsdDecoder`` and exposes common ID3v2.2/v2.3/v2.4 text
tags and APIC/PIC embedded pictures. ``DsfEncoder`` implements ``DsdEncoder``;
it writes ID3v2.4 UTF-8 text tags, including unknown metadata keys as ``TXXX``
values, and APIC picture frames. ID3 does not carry image dimensions, so the
shared picture dimensions, color depth, and indexed-color count are zero when
read from ID3; the writer rejects pictures with nonzero values in those fields.
Both APIs use
``DsdBuffer``'s channel-interleaved octets with most-significant-bit-first bit
order, converting to/from DSF's on-disk bit order. The encoder defaults to the
one-bit storage profile; ``DsfEncoder::new_with_bits_per_sample`` selects the
eight-bit storage profile when needed.

The CLI can inspect DSF and native DSD WavPack files and copy DSD octets
between the two containers with ``zemer convert --format dsf`` or
``--format wavpack``. These paths preserve exact DSF sample counts and do not
pass through PCM.

``DsfDecoder::id3_tag()`` exposes the complete bounded ID3 tag, and
``DsfEncoder::set_id3_tag()`` copies it byte-for-byte, including artwork and
unrecognized frames. CLI DSF-to-DSF conversion uses this path. Cross-format DSD
conversion maps common text tags; target formats that cannot represent ID3
frames cannot preserve artwork or opaque frames.

``dsf::rewrite_metadata`` and ``zemer metadata edit`` replace common text and
picture frames while retaining other opaque frames. Rewriting copies the
DSD data bytes unchanged. Tag-level unsynchronization and extended headers are
normalized away during edits. ID3v2.2 defines no compression scheme, so a tag
with its compression flag set is ignored as a whole and preserved byte-for-byte
by DSF-to-DSF copying.

See the `Sony DSF 1.01 format specification <https://dsd-guide.com/sites/default/files/white-papers/DSFFileFormatSpec_E.pdf>`_
for the container layout and block packing rules.
