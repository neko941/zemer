DSDIFF
======

The ``zemer-dsdiff`` crate reads and writes the uncompressed raw-DSD profile of
DSDIFF (usually ``.dff``). It streams DSD octets directly through the shared
``DsdDecoder`` and ``DsdEncoder`` interfaces; audio never passes through PCM.

Each DSDIFF channel frame stores one octet per channel. The order follows the
``CHNL`` identifiers, and bits are MSB-first within each octet. The decoder
checks the big-endian chunk boundaries, required ``FVER``/``PROP`` fields,
channel count and sound-data alignment before exposing bounded reads. Unknown
chunks are skipped as required by the format. DST-compressed files are
reported as unsupported.

The supported metadata mapping includes one ASCII title, one ASCII artist,
repeated general comments, and the ordered four-byte channel IDs. The
``DsdiffDecoder.comments()`` accessor and ``DsdiffEncoder.set_comments()`` API
also preserve typed ``COMT`` record timestamps, types, references, and text;
only general comments map to the shared ``COMMENT`` field. Comment text is
bounded and restricted to printable ASCII. The writer creates raw DSDIFF
with ``FVER``, ``PROP``, ``FS``, ``CHNL``, ``CMPR`` and ``DSD`` chunks, then
patches the chunk sizes when ``finish()`` is called. DSDIFF stores complete
octets, so it cannot retain a partial final DSD octet; conversion from such a
source returns an error rather than silently changing its sample count.

The CLI can inspect DSDIFF and convert raw DSD among DSDIFF, DSF, and native
DSD WavPack files:

.. code-block:: console

   zemer inspect --json input.dff
   zemer convert --format dsf input.dff output.dsf
   zemer convert --format wavpack input.dff output.wv

This clean-room implementation uses the published `DSDIFF 1.5 specification
<https://www.sonicstudio.com/pdf/dsd/DSDIFF_1.5_Spec.pdf>`_ for format
structure. The published specification states that supplying it grants no
intellectual-property license. No implementation code was copied; a separate
patent or legal clearance review has not been performed.
