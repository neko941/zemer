DSDIFF
======

The ``zemer-dsdiff`` crate reads raw and compressed DST DSDIFF (usually
``.dff``) and writes raw DSD or DST. The default DST encoder applies a
first-order predictor and emits a compressed frame when it is both valid and
smaller; other frames use the specified processing-mode-0 bypass. The
``DsdiffEncoder::new_dst_bypass()`` constructor always selects bypass frames.
It streams DSD octets
directly through the shared ``DsdDecoder`` and ``DsdEncoder`` interfaces;
audio never passes through PCM.

Each DSDIFF channel frame stores one octet per channel. Raw DSD supports the
full 1–65,535-channel range represented by the format's 16-bit channel count;
generated four-byte IDs remain unique across that range. The current DST codec profile
supports up to 64. The order follows the ``CHNL`` identifiers, and bits are
MSB-first within each octet. The decoder
checks the big-endian chunk boundaries, required ``FVER``/``PROP`` fields,
channel count and sound-data alignment before exposing bounded reads. Unknown
chunks are skipped while reading. DST frames are decoded one at a time, and
optional DSDIFF frame CRCs are checked when their audio is read.

The supported metadata mapping includes one ASCII title, one ASCII artist,
repeated general comments, ordered four-byte channel IDs, and typed
``DIIN/MARK`` records exposed by ``DsdiffDecoder.markers()`` and written with
``DsdiffEncoder.set_markers()``. Marker order, signed sample offsets, channel
references, track flags, and descriptions are preserved. Track flags are
checked against the ordered 2/5/6-channel layouts they describe. The
``DsdiffDecoder.comments()`` accessor and ``DsdiffEncoder.set_comments()`` API
also preserve typed ``COMT`` record timestamps, types, references, and text;
only general comments map to the shared ``COMMENT`` field. Comment text is
bounded and restricted to printable ASCII. The writer creates raw DSDIFF
with ``FVER``, ``PROP``, ``FS``, ``CHNL``, ``CMPR`` and ``DSD`` chunks, or a
DST container with ``FRTE`` and one ``DSTF`` per complete 1/75-second frame.
The DST writer accepts DSD64, DSD128, and DSD256 rates and patches the frame
and chunk counts at ``finish()``. DSDIFF stores complete octets, so it cannot retain a partial
final DSD octet; conversion from such a source returns an error rather than
silently changing its sample count. The writer's default mode emits raw DSD;
``DsdiffEncoder::new_dst()`` selects
compressed DST frames or uncompressed bypass frames. Unknown chunks are skipped while reading and
are not retained by either writer. The metadata rewrite path preserves them.

The optional ignored integration tests compare official ISO DST vectors against
their paired raw DSD reference streams. Set ``ZEMER_DST_COMPRESSED_DIR`` to the
ISO ``compressedMp4`` directory and ``ZEMER_DST_REFERENCE_DIR`` to its
``referencesMp4`` directory before running them. ``ZEMER_DST_VECTOR`` optionally
selects one vector by filename. The corpus stays outside this repository. All
three complete 300-frame ``DST_02_02`` sequences (DSD64/128/256) pass bit-exactly,
as do the first 10 frames of every available ISO vector across the tested rates,
channel counts, and test families. The ISO archive does not contain the three
``DST_05`` vectors, so those cases cannot be checked by this external corpus.

``dsdiff::rewrite_metadata`` and ``zemer metadata edit`` update title, artist,
and general comments without decoding or re-encoding audio. The rewrite copies
audio, typed comments, markers, and unchanged or unknown chunks byte-for-byte.
The library API takes a complete metadata snapshot so it can validate and
retain the immutable channel IDs; the CLI constructs this snapshot for you.

The CLI can inspect DSDIFF and convert raw DSD among DSDIFF, DST-compressed
DSDIFF, DSF, and native DSD WavPack files:

.. code-block:: console

   zemer inspect --json input.dff
   zemer convert --format dsf input.dff output.dsf
   zemer convert --format wavpack input.dff output.wv
   zemer convert --format dsdiff_dst input.dsf compressed.dff
   zemer metadata edit --set "TITLE=New title" input.dff updated.dff

When the input and output are both DSDIFF, conversion also carries typed
``COMT`` records and ``DIIN/MARK`` entries. Other output containers retain only
metadata fields they can represent.

This clean-room implementation uses the published `DSDIFF 1.5 specification
<https://www.sonicstudio.com/pdf/dsd/DSDIFF_1.5_Spec.pdf>`_ for format
structure. The published specification states that supplying it grants no
intellectual-property license. No implementation code was copied; a separate
patent or legal clearance review has not been performed.
