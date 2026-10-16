Raw PCM
-------

Headerless PCM has no embedded sample rate, channel count, or sample encoding.
The library therefore requires callers to select the format, sample rate, and
channel count explicitly. It streams unsigned 8-bit PCM, signed 16/24/32-bit
integer PCM, and IEEE float32/64 in explicit little- or big-endian byte order.
Float samples preserve their IEEE bit patterns.

The decoder rejects a final byte sequence that does not contain a complete
interleaved sample frame. Reads and writes are bounded to 16 MiB audio blocks.
There is no metadata in raw PCM.

.. code-block:: console

   zemer inspect --input-format raw_s16le --sample-rate 44100 --channels 2 input.raw
   zemer convert --format wavpack --input-format raw_s16le --sample-rate 44100 --channels 2 input.raw output.wv
   zemer convert --format raw_s24be input.wav output.raw

When writing raw output, the selected ``raw_*`` format determines the target
sample representation and byte order. The source sample rate and channel count
are retained. To decode a raw input, provide all three ``--input-format``,
``--sample-rate``, and ``--channels`` options; they may be used with ``inspect``
or ``convert``.
