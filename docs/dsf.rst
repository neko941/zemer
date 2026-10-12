DSF
===

The ``zemer-dsf`` crate reads and writes Sony DSF 1.01 raw one-bit DSD streams
without converting the samples to PCM. The current profile supports 2.8224 and
5.6448 MHz, mono through 5.1 layouts, and DSF's 4096-byte per-channel block
packing. Channel layout masks are checked before writing so the serialized
channel order remains explicit.

``DsfDecoder`` implements ``DsdDecoder`` and exposes common ID3v2.3/v2.4 text
tags. ``DsfEncoder`` implements ``DsdEncoder``; it writes ID3v2.4 UTF-8 tags,
including unknown metadata keys as ``TXXX`` values. Both APIs use
``DsdBuffer``'s channel-interleaved octets with most-significant-bit-first bit
order, converting to/from DSF's on-disk bit order.

The CLI can inspect DSF and native DSD WavPack files and copy DSD octets
between the two containers with ``zemer convert --format dsf`` or
``--format wavpack``. These paths preserve exact DSF sample counts and do not
pass through PCM.

The current implementation does not retain artwork or unsupported ID3 frames,
so those metadata frames are not preserved when converting.

See the `Sony DSF 1.01 format specification <https://dsd-guide.com/sites/default/files/white-papers/DSFFileFormatSpec_E.pdf>`_
for the container layout and block packing rules.
