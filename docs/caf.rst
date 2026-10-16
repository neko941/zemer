Core Audio Format (CAF)
=======================

The ``zemer-caf`` crate reads and writes a clean-room CAF v1 profile. It uses
the shared ``AudioDecoder`` / ``AudioEncoder`` interfaces and can be used
without the CLI.

The implemented codec profiles are signed linear PCM at 16, 24, and 32 bits;
IEEE float32/64; G.711 A-law and μ-law; and Apple IMA4. PCM supports either
byte order and both packed and high-aligned unpacked 24-bit storage. G.711 and
IMA4 decode to signed 16-bit PCM. Sample rates must be integral and fit the
shared ``AudioSpec`` type. Multichannel streams must use a supported CAF
channel bitmap whose set-bit count matches the channel count. Mono/stereo CAF
files may omit ``chan``.

CAF is a chunk container for many codecs. This profile does not decode AAC,
Apple Lossless, MACE, or user-defined codecs, and it does not interpret custom
per-channel descriptions. Unsupported encodings return an explicit error.
CAF-specific layout tags outside the supported bitmap and standard mono/stereo
tags are rejected so channel order is not silently changed.

The bounded reader requires the CAF v1 header, a first ``desc`` chunk, and
exactly one ``data`` chunk. Unknown-size ``data`` is accepted only as the final
chunk. ``info`` keys and values are parsed as UTF-8 and mapped to shared text
metadata. ``rewrite_metadata`` copies encoded chunk payloads without decoding
or recompressing the audio; an unknown-size data chunk is finalized to its
measured size when rewritten.

CLI examples:

.. code-block:: console

   zemer inspect recording.caf
   zemer convert --format caf input.wav output.caf
   zemer convert --format caf_alaw voice.wav voice-alaw.caf
   zemer convert --format caf_mulaw voice.wav voice-mulaw.caf
   zemer convert --format caf_ima4 input.wav compressed.caf
   zemer metadata edit --set "TITLE=Updated" input.caf tagged.caf

The writer stores a known ``data`` size and writes metadata in the CAF ``info``
chunk. Repeated metadata values are joined with commas as specified by CAF.
CAF does not define escaping for commas inside a value, so those values cannot
round-trip independently through ``info``.

The support boundary follows Apple's `CAF v1 specification
<https://developer.apple.com/library/archive/documentation/MusicAudio/Reference/CAFSpec/CAF_spec/CAF_spec.html>`_;
the implementation and tests are in ``crates/caf/``.
